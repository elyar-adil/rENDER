//! `Temporal.Duration`: the ten-field record, its validity, balancing,
//! rounding and totals, and the class's constructor, statics and methods.

use super::format::{Precision, duration_text};
use super::iso::{IsoDate, NANOS_PER_DAY, add_iso_date, difference_iso_date, epoch_days};
use super::parse::{Component, ParsedDuration, parse_duration};
use super::rounding::{RoundingMode, Unit, increment_is_valid, round_to_increment};
use super::{Surface, TemporalKind, TemporalMethod, TemporalValue, argument, member};
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::value::ObjectHost;
use render_dom::Dom;

/// The ten fields of a Duration, in the order `years` through `nanoseconds`.
/// Each is an integer, held as a Number so that the getters return it as is.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) struct DurationRecord {
    pub(crate) years: f64,
    pub(crate) months: f64,
    pub(crate) weeks: f64,
    pub(crate) days: f64,
    pub(crate) hours: f64,
    pub(crate) minutes: f64,
    pub(crate) seconds: f64,
    pub(crate) milliseconds: f64,
    pub(crate) microseconds: f64,
    pub(crate) nanoseconds: f64,
}

/// The fields as exact integers, in the same order as the record.
type Exact = [i128; 10];

const TWO_POW_32: f64 = 4_294_967_296.0;

/// The length in nanoseconds of the unit each field index stands for. The
/// first three indices are calendar units and have no fixed length.
const FIELD_NANOS: Exact = [
    0,
    0,
    0,
    NANOS_PER_DAY,
    3_600_000_000_000,
    60_000_000_000,
    1_000_000_000,
    1_000_000,
    1_000,
    1,
];

/// The record fields in the order `Duration.from` reads a property bag,
/// with each field's index in [`DurationRecord`].
const DURATION_FIELDS_IN_READ_ORDER: [(&str, usize); 10] = [
    ("days", 3),
    ("hours", 4),
    ("microseconds", 8),
    ("milliseconds", 7),
    ("minutes", 5),
    ("months", 1),
    ("nanoseconds", 9),
    ("seconds", 6),
    ("weeks", 2),
    ("years", 0),
];

const NEEDS_RELATIVE_TO: &str =
    "a relativeTo is required for years, months, weeks or calendar rounding";
const OUT_OF_RANGE: &str = "duration fields are out of range";

impl DurationRecord {
    /// The record whose fields are the given Number values, in field order.
    pub(super) fn from_array(values: [f64; 10]) -> Self {
        Self {
            years: values[0],
            months: values[1],
            weeks: values[2],
            days: values[3],
            hours: values[4],
            minutes: values[5],
            seconds: values[6],
            milliseconds: values[7],
            microseconds: values[8],
            nanoseconds: values[9],
        }
    }

    /// The fields in field order, as Numbers.
    pub(super) fn to_array(self) -> [f64; 10] {
        [
            self.years,
            self.months,
            self.weeks,
            self.days,
            self.hours,
            self.minutes,
            self.seconds,
            self.milliseconds,
            self.microseconds,
            self.nanoseconds,
        ]
    }

    /// The fields as exact integers. Only a valid record reaches here, so
    /// every field is an integer of magnitude below 10^30.
    pub(crate) fn exact(&self) -> Exact {
        self.to_array().map(|value| value as i128)
    }

    /// The sign of the record: -1, 0 or 1. A valid record has one sign.
    pub(crate) fn sign(&self) -> i128 {
        self.to_array()
            .iter()
            .find(|value| **value != 0.0)
            .map_or(0, |value| if *value < 0.0 { -1 } else { 1 })
    }

    /// The time part, hours through nanoseconds, in nanoseconds.
    pub(crate) fn time_nanos(&self) -> i128 {
        let exact = self.exact();
        (4..10).map(|index| exact[index] * FIELD_NANOS[index]).sum()
    }

    /// The largest unit with a non-zero field, or `Nanosecond` for zero.
    pub(crate) fn largest_unit(&self) -> Unit {
        let units = [
            Unit::Year,
            Unit::Month,
            Unit::Week,
            Unit::Day,
            Unit::Hour,
            Unit::Minute,
            Unit::Second,
            Unit::Millisecond,
            Unit::Microsecond,
        ];
        units
            .into_iter()
            .zip(self.to_array())
            .find(|(_, value)| *value != 0.0)
            .map_or(Unit::Nanosecond, |(unit, _)| unit)
    }

    /// The record with each field negated. `0 - x` keeps zero as `+0`.
    pub(crate) fn negated(&self) -> Self {
        Self::from_array(self.to_array().map(|value| 0.0 - value))
    }

    fn absolute(&self) -> Self {
        Self::from_array(self.to_array().map(f64::abs))
    }
}

/// `IsValidDuration`: finite fields of one sign, with years, months and weeks
/// under 2^32 and a total under 2^53 seconds.
pub(crate) fn is_valid(fields: &[f64; 10]) -> bool {
    if fields.iter().any(|value| !value.is_finite()) {
        return false;
    }
    let positive = fields.iter().any(|value| *value > 0.0);
    let negative = fields.iter().any(|value| *value < 0.0);
    if positive && negative {
        return false;
    }
    if fields[..3].iter().any(|value| value.abs() >= TWO_POW_32) {
        return false;
    }
    if fields.iter().any(|value| value.abs() >= 1e30) {
        return false;
    }
    let exact = fields.map(|value| value as i128);
    let mut total: i128 = 0;
    for index in 3..10 {
        let Some(product) = exact[index].checked_mul(FIELD_NANOS[index]) else {
            return false;
        };
        let Some(sum) = total.checked_add(product) else {
            return false;
        };
        total = sum;
    }
    total.abs() < (1_i128 << 53) * 1_000_000_000
}

/// Balances a time total in nanoseconds into the units from `largest` down,
/// returned as `[days, hours, minutes, seconds, ms, us, ns]`. Days appear only
/// when `largest` is a day or larger, and the sign of `total` is kept.
pub(crate) fn balance_time(total: i128, largest: Unit) -> [i128; 7] {
    let chain = [
        Unit::Day,
        Unit::Hour,
        Unit::Minute,
        Unit::Second,
        Unit::Millisecond,
        Unit::Microsecond,
        Unit::Nanosecond,
    ];
    let start = chain.iter().position(|unit| *unit >= largest).unwrap_or(0);
    let sign = if total < 0 { -1 } else { 1 };
    let mut rest = total.abs();
    let mut parts = [0_i128; 7];
    for (index, unit) in chain.iter().enumerate().skip(start) {
        let size = unit.nanos().unwrap_or(1);
        parts[index] = sign * (rest / size);
        rest %= size;
    }
    parts
}

/// The exact fields multiplied by `sign`, as the Number fields of a record.
fn exact_sign_apply(sign: i128, exact: Exact) -> [f64; 10] {
    exact.map(|value| (sign * value) as f64)
}

/// `numerator / denominator` as the nearest Number to the exact quotient.
fn ratio(numerator: i128, denominator: i128) -> f64 {
    numerator as f64 / denominator as f64
}

/// The sign of `one` against `two` as -1, 0 or 1.
fn order(one: i128, two: i128) -> i8 {
    match one.cmp(&two) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// `ValidateTemporalRoundingIncrement`, as a `RangeError` when it fails.
fn check_increment(runtime: &mut JsRuntime, unit: Unit, increment: i128) -> Result<(), JsError> {
    if increment_is_valid(unit, increment) {
        Ok(())
    } else {
        Err(runtime.range_error("roundingIncrement is not valid for this unit"))
    }
}

impl JsRuntime {
    /// The record of `receiver`, or a `TypeError` when it is not a Duration.
    fn duration_of(&self, receiver: ObjectId) -> Result<DurationRecord, JsError> {
        match self.temporal_host(receiver) {
            Some(TemporalValue::Duration(record)) => Ok(record),
            _ => Err(JsError::type_error("receiver is not a Temporal.Duration")),
        }
    }

    /// `CreateTemporalDuration` for a new instance: an invalid record is a
    /// `RangeError`.
    fn duration_value(&mut self, record: DurationRecord) -> Result<JsValue, JsError> {
        if !is_valid(&record.to_array()) {
            return Err(self.range_error(OUT_OF_RANGE));
        }
        self.temporal_instance(TemporalKind::Duration, TemporalValue::Duration(record))
    }

    pub(super) fn duration_construct(
        &mut self,
        dom: &mut Dom,
        new_target: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut values = [0.0_f64; 10];
        for (index, slot) in values.iter_mut().enumerate() {
            let value = argument(arguments, index);
            if !matches!(value, JsValue::Undefined) {
                *slot = self.to_integer_if_integral(dom, &value)? + 0.0;
            }
        }
        if !is_valid(&values) {
            return Err(self.range_error(OUT_OF_RANGE));
        }
        let prototype = self.temporal_prototype_for(dom, TemporalKind::Duration, new_target)?;
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.allocate_host(
            Some(prototype),
            ObjectHost::Temporal(TemporalValue::Duration(DurationRecord::from_array(values))),
        )))
    }

    /// `ToTemporalDurationRecord`: a Duration, a duration string, or a
    /// property bag with at least one duration field.
    pub(super) fn to_temporal_duration(
        &mut self,
        dom: &mut Dom,
        item: &JsValue,
    ) -> Result<DurationRecord, JsError> {
        match item {
            JsValue::Object(object) => {
                if let Some(TemporalValue::Duration(record)) = self.temporal_host(*object) {
                    return Ok(record);
                }
                let mut values = [0.0_f64; 10];
                let mut any = false;
                for (name, index) in DURATION_FIELDS_IN_READ_ORDER {
                    let value = self.get_member(dom, *object, name)?;
                    if !matches!(value, JsValue::Undefined) {
                        any = true;
                        values[index] = self.to_integer_if_integral(dom, &value)? + 0.0;
                    }
                }
                if !any {
                    return Err(JsError::type_error(
                        "a duration-like object needs at least one duration field",
                    ));
                }
                if !is_valid(&values) {
                    return Err(self.range_error(OUT_OF_RANGE));
                }
                Ok(DurationRecord::from_array(values))
            }
            JsValue::String(text) => {
                let Some(parsed) = parse_duration(text) else {
                    return Err(self.range_error("invalid duration string"));
                };
                self.duration_from_string(&parsed)
            }
            _ => Err(JsError::type_error(
                "expected a Duration, a duration string or a duration-like object",
            )),
        }
    }

    /// The record a parsed duration string names. A fraction on the last
    /// component carries into the smaller time fields exactly.
    fn duration_from_string(&mut self, parsed: &ParsedDuration) -> Result<DurationRecord, JsError> {
        let slots: [(usize, &Option<Component>); 7] = [
            (0, &parsed.years),
            (1, &parsed.months),
            (2, &parsed.weeks),
            (3, &parsed.days),
            (4, &parsed.hours),
            (5, &parsed.minutes),
            (6, &parsed.seconds),
        ];
        let mut exact: Exact = [0; 10];
        for (index, component) in slots {
            let Some(component) = component else { continue };
            exact[index] = component.whole;
            if component.fraction.is_empty() {
                continue;
            }
            // Only the time components may carry a fraction.
            if index < 4 {
                return Err(self.range_error("only a time component may have a fraction"));
            }
            let digits = u32::try_from(component.fraction.len()).unwrap_or(9);
            let digit_value: i128 = component.fraction.parse().unwrap_or(0);
            let mut rest = digit_value * FIELD_NANOS[index] / 10_i128.pow(digits);
            for slot in index + 1..10 {
                exact[slot] += rest / FIELD_NANOS[slot];
                rest %= FIELD_NANOS[slot];
            }
        }
        let sign: i128 = if parsed.negative { -1 } else { 1 };
        let fields = exact_sign_apply(sign, exact);
        if !is_valid(&fields) {
            return Err(self.range_error(OUT_OF_RANGE));
        }
        Ok(DurationRecord::from_array(fields))
    }

    /// `AddDuration` for `add` and `subtract`. Calendar fields need a
    /// relative date, which these methods do not take, so they are refused.
    fn add_durations(
        &mut self,
        first: DurationRecord,
        second: DurationRecord,
    ) -> Result<DurationRecord, JsError> {
        let calendar = |record: &DurationRecord| {
            record.years != 0.0 || record.months != 0.0 || record.weeks != 0.0
        };
        if calendar(&first) || calendar(&second) {
            return Err(self.range_error(NEEDS_RELATIVE_TO));
        }
        // The time total is balanced up to whole days, which absorb into the
        // day field, and the rest into hours and below.
        let days = first.days as i128 + second.days as i128;
        let parts = balance_time(first.time_nanos() + second.time_nanos(), Unit::Day);
        let exact = [
            0,
            0,
            0,
            days + parts[0],
            parts[1],
            parts[2],
            parts[3],
            parts[4],
            parts[5],
            parts[6],
        ];
        let fields = exact.map(|value| value as f64);
        if !is_valid(&fields) {
            return Err(self.range_error("duration result is out of range"));
        }
        Ok(DurationRecord::from_array(fields))
    }

    /// `relativeTo` as an option: a date, read through `to_relative_date`.
    pub(super) fn relative_to_option(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
    ) -> Result<Option<IsoDate>, JsError> {
        let value = self.option_value(dom, options, "relativeTo")?;
        if matches!(value, JsValue::Undefined) {
            return Ok(None);
        }
        self.to_relative_date(dom, &value).map(Some)
    }

    /// The ISO date a `relativeTo` value stands for, with `constrain` overflow.
    pub(super) fn to_relative_date(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<IsoDate, JsError> {
        match value {
            // A relativeTo string may carry an offset or `Z`: its local date is
            // the one used, and the offset and any zone name do not change it.
            JsValue::String(text) => {
                let Some(parsed) = super::parse::parse_date_time(text) else {
                    return Err(self.range_error("invalid relativeTo string"));
                };
                if parsed
                    .calendar
                    .as_deref()
                    .is_some_and(|calendar| calendar != "iso8601")
                {
                    return Err(self.range_error("only the iso8601 calendar is supported"));
                }
                super::iso::regulate_date(
                    parsed.date.year,
                    i64::from(parsed.date.month),
                    i64::from(parsed.date.day),
                    true,
                )
                .filter(|date| super::iso::date_within_limits(*date))
                .ok_or_else(|| self.range_error("relativeTo is out of range"))
            }
            // A property bag contributes its time fields too, which are read and
            // converted even though only the date is kept.
            JsValue::Object(object) if self.temporal_host(*object).is_none() => {
                let (bag, time) = self.read_datetime_bag(dom, *object)?;
                let date = self.date_from_bag(&bag, true)?;
                self.time_from_bag(&time, true)?;
                Ok(date)
            }
            _ => self.to_relative_plain_date(dom, value),
        }
    }

    /// `RoundDuration` for a record already read: the rounding of `unit` to
    /// `increment`, balanced up to `largest`. The arithmetic is signed and
    /// measured from `relative_to`, so a negative record is rounded backwards
    /// from it, as the specification's `NudgeToCalendarUnit` does.
    pub(super) fn round_record(
        &mut self,
        record: DurationRecord,
        largest: Unit,
        unit: Unit,
        increment: i128,
        mode: RoundingMode,
        relative_to: Option<IsoDate>,
    ) -> Result<DurationRecord, JsError> {
        let sign = record.sign();
        if sign == 0 {
            return Ok(DurationRecord::default());
        }
        let exact = record.exact();
        let time = record.time_nanos();
        let calendar_unit = matches!(unit, Unit::Year | Unit::Month | Unit::Week);
        let calendar_largest = matches!(largest, Unit::Year | Unit::Month);
        let calendar_present = exact[0] != 0 || exact[1] != 0 || exact[2] != 0;
        let Some(relative) = relative_to else {
            // Weeks are a calendar unit here too: without a relative date, only a
            // record whose largest unit is no larger than a day can be balanced.
            if calendar_unit || calendar_largest || calendar_present || largest == Unit::Week {
                return Err(self.range_error(NEEDS_RELATIVE_TO));
            }
            return self.round_fixed_record(record, largest, unit, increment, mode);
        };
        if calendar_unit {
            let fields = [exact[0], exact[1], exact[2], exact[3], time];
            let destination =
                self.round_calendar_unit(relative, sign, fields, unit, increment, mode)?;
            let (years, months, weeks, days) = difference_iso_date(relative, destination, largest);
            return self.signed_record(
                1,
                [
                    i128::from(years),
                    i128::from(months),
                    i128::from(weeks),
                    i128::from(days),
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                ],
            );
        }
        // A calendar part that the result does not keep is folded into days.
        let (folded_days, rest) = if largest.nanos().is_some() {
            let folded = add_iso_date(
                relative,
                exact[0] as i64,
                exact[1] as i64,
                exact[2] as i64,
                0,
                true,
            )
            .ok_or_else(|| self.range_error(OUT_OF_RANGE))?;
            let extra = i128::from(epoch_days(folded) - epoch_days(relative));
            (exact[3] + extra, [0_i64; 3])
        } else {
            (
                exact[3],
                [exact[0] as i64, exact[1] as i64, exact[2] as i64],
            )
        };
        let unit_nanos = unit.nanos().unwrap_or(NANOS_PER_DAY);
        let rounded = round_to_increment(
            folded_days * NANOS_PER_DAY + time,
            unit_nanos * increment,
            mode,
        );
        let balance_from = if largest.nanos().is_some() {
            largest
        } else {
            Unit::Day
        };
        let parts = balance_time(rounded, balance_from);
        let total_days = parts[0];
        let mut result = [0_i128; 10];
        if calendar_largest || (largest == Unit::Week && calendar_present) {
            let end = add_iso_date(relative, rest[0], rest[1], rest[2], total_days as i64, true)
                .ok_or_else(|| self.range_error(OUT_OF_RANGE))?;
            let (years, months, weeks, days) = difference_iso_date(relative, end, largest);
            result[..4].copy_from_slice(&[
                i128::from(years),
                i128::from(months),
                i128::from(weeks),
                i128::from(days),
            ]);
        } else if largest == Unit::Week {
            let weeks = total_days / 7;
            result[2] = weeks;
            result[3] = total_days - weeks * 7;
        } else {
            result[3] = total_days;
        }
        result[4..].copy_from_slice(&parts[1..]);
        self.signed_record(1, result)
    }

    /// Rounds a record whose calendar fields are absent and whose largest
    /// unit is no larger than a week: a fixed-length rounding.
    fn round_fixed_record(
        &mut self,
        record: DurationRecord,
        largest: Unit,
        unit: Unit,
        increment: i128,
        mode: RoundingMode,
    ) -> Result<DurationRecord, JsError> {
        let value = record.exact()[3] * NANOS_PER_DAY + record.time_nanos();
        let unit_nanos = unit.nanos().unwrap_or(NANOS_PER_DAY);
        let rounded = round_to_increment(value, unit_nanos * increment, mode);
        let balance_from = if largest.nanos().is_some() {
            largest
        } else {
            Unit::Day
        };
        let parts = balance_time(rounded, balance_from);
        let mut result = [0_i128; 10];
        if largest == Unit::Week {
            result[2] = parts[0] / 7;
            result[3] = parts[0] % 7;
        } else {
            result[3] = parts[0];
        }
        result[4..].copy_from_slice(&parts[1..]);
        self.signed_record(1, result)
    }

    /// `NudgeToCalendarUnit` for a calendar unit: the count of `unit` nearest the
    /// destination of `fields` (years, months, weeks, days and time), measured
    /// in signed arithmetic from `relative`. Returns the date the rounded count
    /// lands on.
    fn round_calendar_unit(
        &mut self,
        relative: IsoDate,
        sign: i128,
        [years, months, weeks, days, time]: [i128; 5],
        unit: Unit,
        increment: i128,
        mode: RoundingMode,
    ) -> Result<IsoDate, JsError> {
        let magnitude = match unit {
            Unit::Year => years.abs(),
            Unit::Month => months.abs(),
            _ => weeks.abs(),
        };
        let base = magnitude / increment * increment;
        let step = |count: i128| (sign * count) as i64;
        let (start_parts, end_parts) = match unit {
            Unit::Year => ([step(base), 0, 0, 0], [step(base + increment), 0, 0, 0]),
            Unit::Month => (
                [years as i64, step(base), 0, 0],
                [years as i64, step(base + increment), 0, 0],
            ),
            _ => (
                [years as i64, months as i64, step(base), 0],
                [years as i64, months as i64, step(base + increment), 0],
            ),
        };
        let start = add_iso_date(
            relative,
            start_parts[0],
            start_parts[1],
            start_parts[2],
            start_parts[3],
            true,
        );
        let end = add_iso_date(
            relative,
            end_parts[0],
            end_parts[1],
            end_parts[2],
            end_parts[3],
            true,
        );
        let destination = add_iso_date(
            relative,
            years as i64,
            months as i64,
            weeks as i64,
            days as i64,
            true,
        );
        let (Some(start), Some(end), Some(destination)) = (start, end, destination) else {
            return Err(self.range_error(OUT_OF_RANGE));
        };
        let start_nanos = i128::from(epoch_days(start)) * NANOS_PER_DAY;
        let end_nanos = i128::from(epoch_days(end)) * NANOS_PER_DAY;
        let mut value = i128::from(epoch_days(destination)) * NANOS_PER_DAY + time - start_nanos;
        let mut span = end_nanos - start_nanos;
        // A span running backwards is rounded as its mirror image.
        let mode = if span < 0 {
            value = -value;
            span = -span;
            mode.negated()
        } else {
            mode
        };
        if span == 0 {
            return Err(self.range_error(OUT_OF_RANGE));
        }
        let steps = round_to_increment(value, span, mode) / span;
        let counted = (sign * (base + increment * steps)) as i64;
        let result = match unit {
            Unit::Year => add_iso_date(relative, counted, 0, 0, 0, true),
            Unit::Month => add_iso_date(relative, years as i64, counted, 0, 0, true),
            _ => add_iso_date(relative, years as i64, months as i64, counted, 0, true),
        };
        result.ok_or_else(|| self.range_error(OUT_OF_RANGE))
    }

    /// The record for `result` with `sign` applied, checked for validity.
    fn signed_record(&mut self, sign: i128, result: Exact) -> Result<DurationRecord, JsError> {
        let fields = exact_sign_apply(sign, result);
        if !is_valid(&fields) {
            return Err(self.range_error(OUT_OF_RANGE));
        }
        Ok(DurationRecord::from_array(fields))
    }

    /// `Temporal.Duration.prototype.round`. `roundTo` is a unit string or an
    /// options object with `largestUnit`, `relativeTo`, `roundingIncrement`,
    /// `roundingMode` and `smallestUnit`, read in that order.
    fn duration_round(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let record = self.duration_of(receiver)?;
        let round_to = argument(arguments, 0);
        let (largest, relative_to, increment, mode, smallest) = match &round_to {
            JsValue::Undefined => {
                return Err(JsError::type_error("round requires an options argument"));
            }
            JsValue::String(text) => {
                let Some(smallest) = Unit::parse(text) else {
                    return Err(self.range_error("invalid smallestUnit"));
                };
                (None, None, 1, RoundingMode::HalfExpand, Some(smallest))
            }
            other => {
                let options = Self::options_object(other)?;
                let largest = self.unit_option(dom, options, "largestUnit", true)?;
                let relative_to = self.relative_to_option(dom, options)?;
                let increment = self.rounding_increment_option(dom, options)?;
                let mode = self.rounding_mode_option(dom, options, RoundingMode::HalfExpand)?;
                let smallest = self.unit_option(dom, options, "smallestUnit", false)?;
                (largest, relative_to, increment, mode, smallest)
            }
        };
        if largest.is_none() && smallest.is_none() {
            return Err(self.range_error("round requires largestUnit or smallestUnit"));
        }
        let unit = smallest.unwrap_or(Unit::Nanosecond);
        check_increment(self, unit, increment)?;
        let largest = match largest {
            Some(largest) if largest > unit => {
                return Err(self.range_error("largestUnit must not be smaller than smallestUnit"));
            }
            Some(largest) => largest,
            None => record.largest_unit().min(unit),
        };
        // A calendar unit cannot be rounded to an increment while the result
        // balances up past it.
        if matches!(unit, Unit::Year | Unit::Month | Unit::Week) && increment != 1 && largest < unit
        {
            return Err(self.range_error(
                "cannot round to an increment of a calendar unit while balancing up",
            ));
        }
        let rounded = self.round_record(record, largest, unit, increment, mode, relative_to)?;
        self.duration_value(rounded)
    }

    /// `Temporal.Duration.prototype.total`: the number of `unit` in the
    /// duration. `totalOf` is a unit string or an options object with `unit`
    /// and `relativeTo`.
    fn duration_total(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let record = self.duration_of(receiver)?;
        let total_of = argument(arguments, 0);
        let (unit, relative_to) = match &total_of {
            JsValue::Undefined => {
                return Err(JsError::type_error("total requires an argument"));
            }
            JsValue::String(text) => (Unit::parse(text), None),
            other => {
                let options = Self::options_object(other)?;
                let relative_to = self.relative_to_option(dom, options)?;
                let unit = self.unit_option(dom, options, "unit", false)?;
                (unit, relative_to)
            }
        };
        let Some(unit) = unit else {
            return Err(self.range_error("total requires a unit"));
        };
        Ok(JsValue::Number(self.total_of(record, unit, relative_to)?))
    }

    /// The number of `unit` in `record`, signed. A calendar unit counts whole
    /// units from `relative_to` and takes the fraction of the next one, so the
    /// fraction is exact before the one rounding to a Number.
    fn total_of(
        &mut self,
        record: DurationRecord,
        unit: Unit,
        relative_to: Option<IsoDate>,
    ) -> Result<f64, JsError> {
        let sign = record.sign();
        if sign == 0 {
            return Ok(0.0);
        }
        let exact = record.exact();
        let time = record.time_nanos();
        let (years, months, weeks, days) = (exact[0], exact[1], exact[2], exact[3]);
        let calendar = years != 0 || months != 0 || weeks != 0;
        let calendar_unit = matches!(unit, Unit::Year | Unit::Month | Unit::Week);
        if !calendar && !calendar_unit {
            let total = days * NANOS_PER_DAY + time;
            return Ok(ratio(total, unit.nanos().unwrap_or(NANOS_PER_DAY)));
        }
        let Some(relative) = relative_to else {
            return Err(self.range_error(NEEDS_RELATIVE_TO));
        };
        let destination = add_iso_date(
            relative,
            years as i64,
            months as i64,
            weeks as i64,
            days as i64,
            true,
        )
        .ok_or_else(|| self.range_error(OUT_OF_RANGE))?;
        let origin = i128::from(epoch_days(relative)) * NANOS_PER_DAY;
        let destination_nanos = i128::from(epoch_days(destination)) * NANOS_PER_DAY + time;
        if !matches!(unit, Unit::Year | Unit::Month) {
            let unit_nanos = unit.nanos().unwrap_or(7 * NANOS_PER_DAY);
            return Ok(ratio(destination_nanos - origin, unit_nanos));
        }
        let step = sign as i64;
        let (whole, start, end) = if unit == Unit::Year {
            (
                years,
                [years as i64, 0, 0, 0],
                [(years + i128::from(step)) as i64, 0, 0, 0],
            )
        } else {
            (
                years * 12 + months,
                [years as i64, months as i64, 0, 0],
                [years as i64, (months + i128::from(step)) as i64, 0, 0],
            )
        };
        let start = add_iso_date(relative, start[0], start[1], start[2], start[3], true);
        let end = add_iso_date(relative, end[0], end[1], end[2], end[3], true);
        let (Some(start), Some(end)) = (start, end) else {
            return Err(self.range_error(OUT_OF_RANGE));
        };
        let start_nanos = i128::from(epoch_days(start)) * NANOS_PER_DAY;
        let end_nanos = i128::from(epoch_days(end)) * NANOS_PER_DAY;
        let fraction = ratio(destination_nanos - start_nanos, end_nanos - start_nanos);
        Ok(whole as f64 + f64::from(sign as i8) * fraction)
    }

    /// `CompareDurations`: -1, 0 or 1. Durations with calendar fields are
    /// compared from `relative_to`, which they then require.
    fn compare_durations(
        &mut self,
        one: DurationRecord,
        two: DurationRecord,
        relative_to: Option<IsoDate>,
    ) -> Result<i8, JsError> {
        let calendar = |record: &DurationRecord| {
            record.years != 0.0 || record.months != 0.0 || record.weeks != 0.0
        };
        if !calendar(&one) && !calendar(&two) {
            let first = one.days as i128 * NANOS_PER_DAY + one.time_nanos();
            let second = two.days as i128 * NANOS_PER_DAY + two.time_nanos();
            return Ok(order(first, second));
        }
        let Some(relative) = relative_to else {
            return Err(self.range_error(NEEDS_RELATIVE_TO));
        };
        let origin = i128::from(epoch_days(relative)) * NANOS_PER_DAY;
        let mut ends = [0_i128; 2];
        for (slot, record) in ends.iter_mut().zip([one, two]) {
            let exact = record.exact();
            let date = add_iso_date(
                relative,
                exact[0] as i64,
                exact[1] as i64,
                exact[2] as i64,
                exact[3] as i64,
                true,
            )
            .ok_or_else(|| self.range_error(OUT_OF_RANGE))?;
            *slot = i128::from(epoch_days(date)) * NANOS_PER_DAY + record.time_nanos() - origin;
        }
        Ok(order(ends[0], ends[1]))
    }

    /// `with(durationLike)`: the record with each named field replaced.
    fn partial_duration(
        &mut self,
        dom: &mut Dom,
        item: &JsValue,
        record: DurationRecord,
    ) -> Result<DurationRecord, JsError> {
        let JsValue::Object(object) = item else {
            return Err(JsError::type_error(
                "with() requires a duration-like object",
            ));
        };
        let mut values = record.to_array();
        let mut any = false;
        for (name, index) in DURATION_FIELDS_IN_READ_ORDER {
            let value = self.get_member(dom, *object, name)?;
            if !matches!(value, JsValue::Undefined) {
                any = true;
                values[index] = self.to_integer_if_integral(dom, &value)? + 0.0;
            }
        }
        if !any {
            return Err(JsError::type_error(
                "with() requires at least one duration field",
            ));
        }
        if !is_valid(&values) {
            return Err(self.range_error(OUT_OF_RANGE));
        }
        Ok(DurationRecord::from_array(values))
    }

    /// `toString` and `toJSON`. The options are read in the specified order:
    /// `fractionalSecondDigits`, `roundingMode`, `smallestUnit`. Rounding acts
    /// on the time part, which is then balanced up to hours.
    fn duration_to_string(
        &mut self,
        dom: &mut Dom,
        record: DurationRecord,
        options: &JsValue,
    ) -> Result<String, JsError> {
        let options = Self::options_object(options)?;
        let digits = self.fractional_digits_option(dom, options)?;
        let mode = self.rounding_mode_option(dom, options, RoundingMode::Trunc)?;
        let smallest = self.unit_option(dom, options, "smallestUnit", false)?;
        // `toString` takes a second or a finer unit only.
        let precision = match (smallest, digits) {
            (Some(Unit::Second), _) => Precision::Digits(0),
            (Some(Unit::Millisecond), _) => Precision::Digits(3),
            (Some(Unit::Microsecond), _) => Precision::Digits(6),
            (Some(Unit::Nanosecond), _) => Precision::Digits(9),
            (Some(_), _) => return Err(self.range_error("smallestUnit is not a time unit")),
            (None, Some(count)) => Precision::Digits(count),
            (None, None) => Precision::Auto,
        };
        let exact = record.exact();
        let mut shown = exact.map(i128::unsigned_abs);
        if let Precision::Digits(count) = precision {
            // Rounding the time part may carry whole days out of it, which join
            // the days field: the result balances up to days, not hours.
            let magnitude = record.absolute();
            let rounded = round_to_increment(
                magnitude.time_nanos(),
                10_i128.pow(9 - u32::from(count)),
                mode,
            );
            let parts = balance_time(rounded, Unit::Day);
            shown[3] = exact[3].unsigned_abs() + parts[0].unsigned_abs();
            for (slot, part) in shown[4..].iter_mut().zip(&parts[1..]) {
                *slot = part.unsigned_abs();
            }
        }
        Ok(duration_text(shown, record.sign() < 0, precision))
    }

    /// The Duration methods and statics, dispatched by name.
    pub(super) fn duration_native(
        &mut self,
        dom: &mut Dom,
        method: TemporalMethod,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let first = argument(arguments, 0);
        match method {
            TemporalMethod::From => {
                let record = self.to_temporal_duration(dom, &first)?;
                self.duration_value(record)
            }
            TemporalMethod::Compare => {
                let one = self.to_temporal_duration(dom, &first)?;
                let two = self.to_temporal_duration(dom, &argument(arguments, 1))?;
                let options = Self::options_object(&argument(arguments, 2))?;
                let relative_to = self.relative_to_option(dom, options)?;
                let order = self.compare_durations(one, two, relative_to)?;
                Ok(JsValue::Number(f64::from(order)))
            }
            TemporalMethod::Add | TemporalMethod::Subtract => {
                let record = self.duration_of(receiver)?;
                let mut other = self.to_temporal_duration(dom, &first)?;
                if method == TemporalMethod::Subtract {
                    other = other.negated();
                }
                let sum = self.add_durations(record, other)?;
                self.duration_value(sum)
            }
            TemporalMethod::Negated => {
                let record = self.duration_of(receiver)?;
                self.duration_value(record.negated())
            }
            TemporalMethod::Abs => {
                let record = self.duration_of(receiver)?;
                self.duration_value(record.absolute())
            }
            TemporalMethod::With => {
                let record = self.duration_of(receiver)?;
                let fields = self.partial_duration(dom, &first, record)?;
                self.duration_value(fields)
            }
            TemporalMethod::Round => self.duration_round(dom, receiver, arguments),
            TemporalMethod::Total => self.duration_total(dom, receiver, arguments),
            TemporalMethod::ToString => {
                let record = self.duration_of(receiver)?;
                let text = self.duration_to_string(dom, record, &first)?;
                Ok(JsValue::String(text))
            }
            // `toJSON` and `toLocaleString` take no options, so none are read.
            TemporalMethod::ToJSON | TemporalMethod::ToLocaleString => {
                let record = self.duration_of(receiver)?;
                let text = self.duration_to_string(dom, record, &JsValue::Undefined)?;
                Ok(JsValue::String(text))
            }
            TemporalMethod::ValueOf => Err(JsError::type_error(
                "Temporal.Duration cannot be converted to a primitive; use compare() or toString()",
            )),
            other => self.duration_getter(receiver, other),
        }
    }

    /// The Duration getters, which read one field or the sign.
    fn duration_getter(
        &mut self,
        receiver: ObjectId,
        method: TemporalMethod,
    ) -> Result<JsValue, JsError> {
        let record = self.duration_of(receiver)?;
        let value = match method {
            TemporalMethod::Years => record.years,
            TemporalMethod::Months => record.months,
            TemporalMethod::Weeks => record.weeks,
            TemporalMethod::Days => record.days,
            TemporalMethod::Hours => record.hours,
            TemporalMethod::Minutes => record.minutes,
            TemporalMethod::Seconds => record.seconds,
            TemporalMethod::Milliseconds => record.milliseconds,
            TemporalMethod::Microseconds => record.microseconds,
            TemporalMethod::Nanoseconds => record.nanoseconds,
            TemporalMethod::Sign => record.sign() as f64,
            TemporalMethod::Blank => return Ok(JsValue::Boolean(record.sign() == 0)),
            _ => return Err(JsError::type_error("unsupported Temporal.Duration member")),
        };
        Ok(JsValue::Number(value))
    }
}

/// The members of `Temporal.Duration` and its prototype.
pub(super) const SURFACE: Surface = Surface {
    constructor_length: Some(0),
    statics: &[
        member("from", TemporalMethod::From, 1),
        member("compare", TemporalMethod::Compare, 2),
    ],
    methods: &[
        member("with", TemporalMethod::With, 1),
        member("negated", TemporalMethod::Negated, 0),
        member("abs", TemporalMethod::Abs, 0),
        member("add", TemporalMethod::Add, 1),
        member("subtract", TemporalMethod::Subtract, 1),
        member("round", TemporalMethod::Round, 1),
        member("total", TemporalMethod::Total, 1),
        member("toString", TemporalMethod::ToString, 0),
        member("toJSON", TemporalMethod::ToJSON, 0),
        member("toLocaleString", TemporalMethod::ToLocaleString, 0),
        member("valueOf", TemporalMethod::ValueOf, 0),
    ],
    getters: &[
        ("years", TemporalMethod::Years),
        ("months", TemporalMethod::Months),
        ("weeks", TemporalMethod::Weeks),
        ("days", TemporalMethod::Days),
        ("hours", TemporalMethod::Hours),
        ("minutes", TemporalMethod::Minutes),
        ("seconds", TemporalMethod::Seconds),
        ("milliseconds", TemporalMethod::Milliseconds),
        ("microseconds", TemporalMethod::Microseconds),
        ("nanoseconds", TemporalMethod::Nanoseconds),
        ("sign", TemporalMethod::Sign),
        ("blank", TemporalMethod::Blank),
    ],
};
