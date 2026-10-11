//! `Temporal.Instant`: a point on the UTC timeline, held as epoch nanoseconds.
//! Its text is always UTC, so no time zone is ever resolved here.

use super::duration::{DurationRecord, balance_time};
use super::format::{Precision, instant_text};
use super::iso::instant_within_limits;
use super::options::{DifferenceSettings, UnitGroup};
use super::parse::parse_instant;
use super::rounding::{RoundingMode, Unit, increment_is_valid, round_to_increment};
use super::{Surface, TemporalKind, TemporalMethod, TemporalValue, argument, member};
use crate::JsBigInt;
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use render_dom::Dom;

const NANOS_PER_MILLI: i128 = 1_000_000;

/// The nanoseconds that `toString` keeps and the increment it rounds to, for
/// the precision a `smallestUnit` or `fractionalSecondDigits` asks for. The
/// caller has already refused `hour`, which `toString` does not accept.
pub(super) fn seconds_precision(smallest: Option<Unit>, digits: Option<u8>) -> (Precision, i128) {
    match (smallest, digits) {
        (Some(Unit::Minute), _) => (Precision::Minute, 60_000_000_000),
        (Some(Unit::Second), _) => (Precision::Digits(0), 1_000_000_000),
        (Some(Unit::Millisecond), _) => (Precision::Digits(3), 1_000_000),
        (Some(Unit::Microsecond), _) => (Precision::Digits(6), 1_000),
        (Some(Unit::Nanosecond), _) => (Precision::Digits(9), 1),
        (Some(_), _) | (None, None) => (Precision::Auto, 1),
        (None, Some(count)) => (Precision::Digits(count), 10_i128.pow(9 - u32::from(count))),
    }
}

/// The exact epoch nanoseconds of a `BigInt`, or `None` when it is too large
/// to be any instant.
fn nanos_of_bigint(value: &JsBigInt) -> Option<i128> {
    value.to_string_radix(10).parse::<i128>().ok()
}

/// A `BigInt` with the value of `nanos`, negative values included.
fn bigint_of(nanos: i128) -> JsBigInt {
    let magnitude = JsBigInt::parse_digits(&nanos.unsigned_abs().to_string(), 10)
        .unwrap_or_else(JsBigInt::zero);
    if nanos < 0 {
        magnitude.negate()
    } else {
        magnitude
    }
}

impl JsRuntime {
    /// The epoch nanoseconds of `receiver`, or a `TypeError` for another value.
    fn instant_of(&self, receiver: ObjectId) -> Result<i128, JsError> {
        match self.temporal_host(receiver) {
            Some(TemporalValue::Instant(nanos)) => Ok(nanos),
            _ => Err(JsError::type_error("receiver is not a Temporal.Instant")),
        }
    }

    /// A new Instant of `nanos`, refused when outside the representable range.
    fn instant_value(&mut self, nanos: i128) -> Result<JsValue, JsError> {
        if !instant_within_limits(nanos) {
            return Err(self.range_error("Temporal.Instant is outside the representable range"));
        }
        self.temporal_instance(TemporalKind::Instant, TemporalValue::Instant(nanos))
    }

    pub(super) fn instant_construct(
        &mut self,
        dom: &mut Dom,
        new_target: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let value = self.to_bigint_value(dom, &argument(arguments, 0))?;
        let Some(nanos) = nanos_of_bigint(&value).filter(|nanos| instant_within_limits(*nanos))
        else {
            return Err(self.range_error("Temporal.Instant is outside the representable range"));
        };
        let prototype = self.temporal_prototype_for(dom, TemporalKind::Instant, new_target)?;
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.allocate_host(
            Some(prototype),
            crate::value::ObjectHost::Temporal(TemporalValue::Instant(nanos)),
        )))
    }

    /// `ToTemporalInstant`: an Instant, or a string (or object whose string
    /// form) with a mandatory UTC offset. Any other primitive is a `TypeError`.
    pub(super) fn to_temporal_instant(
        &mut self,
        dom: &mut Dom,
        item: &JsValue,
    ) -> Result<i128, JsError> {
        match item {
            JsValue::Object(object) => {
                if let Some(TemporalValue::Instant(nanos)) = self.temporal_host(*object) {
                    return Ok(nanos);
                }
                let text = self.to_string_checked(dom, item)?;
                self.instant_from_text(&text)
            }
            JsValue::String(text) => self.instant_from_text(text),
            _ => Err(JsError::type_error(
                "expected a Temporal.Instant or an ISO string",
            )),
        }
    }

    /// The instant an ISO string names: its local date and time minus its
    /// offset. The time zone annotation, if any, does not change the instant.
    fn instant_from_text(&mut self, text: &str) -> Result<i128, JsError> {
        let Some(parsed) = parse_instant(text) else {
            return Err(self.range_error("invalid ISO instant string"));
        };
        if parsed
            .calendar
            .as_deref()
            .is_some_and(|calendar| calendar != "iso8601")
        {
            return Err(self.range_error("only the iso8601 calendar is supported"));
        }
        let Some(time) = parsed.time else {
            return Err(self.range_error("an instant needs a time"));
        };
        if !super::iso::is_valid_date(
            parsed.date.year,
            i64::from(parsed.date.month),
            i64::from(parsed.date.day),
        ) {
            return Err(self.range_error("the instant's date does not exist"));
        }
        let local = super::iso::datetime_epoch_nanos(super::iso::IsoDateTime {
            date: parsed.date,
            time,
        });
        let nanos = local - parsed.offset.unwrap_or(0);
        if !instant_within_limits(nanos) {
            return Err(self.range_error("Temporal.Instant is outside the representable range"));
        }
        Ok(nanos)
    }

    /// `AddDurationToInstant`: the time fields of `duration` added to `nanos`.
    /// A calendar or day field has no fixed length, so it is a `RangeError`.
    fn instant_add(
        &mut self,
        dom: &mut Dom,
        nanos: i128,
        item: &JsValue,
        sign: i128,
    ) -> Result<JsValue, JsError> {
        let duration = self.to_temporal_duration(dom, item)?;
        if duration.years != 0.0
            || duration.months != 0.0
            || duration.weeks != 0.0
            || duration.days != 0.0
        {
            return Err(self.range_error("Temporal.Instant arithmetic takes only time units"));
        }
        let Some(sum) = nanos.checked_add(sign * duration.time_nanos()) else {
            return Err(self.range_error("Temporal.Instant is outside the representable range"));
        };
        self.instant_value(sum)
    }

    /// `DifferenceInstant`: the time from `from` to `to`, rounded and balanced
    /// by `settings`, as a Duration record.
    fn difference_instant(
        from: i128,
        to: i128,
        settings: DifferenceSettings,
        since: bool,
    ) -> DurationRecord {
        let unit_nanos = settings.smallest.nanos().unwrap_or(1);
        let rounded = round_to_increment(to - from, unit_nanos * settings.increment, settings.mode);
        let signed = if since { -rounded } else { rounded };
        let parts = balance_time(signed, settings.largest);
        DurationRecord::from_array([
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
        ])
    }

    /// `until` and `since` on two instants.
    fn instant_difference(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        since: bool,
    ) -> Result<JsValue, JsError> {
        let from = self.instant_of(receiver)?;
        let to = self.to_temporal_instant(dom, &argument(arguments, 0))?;
        let options = Self::options_object(&argument(arguments, 1))?;
        let settings = self.difference_settings(
            dom,
            options,
            since,
            UnitGroup::Time,
            Unit::Nanosecond,
            Unit::Second,
        )?;
        let record = Self::difference_instant(from, to, settings, since);
        self.temporal_instance(TemporalKind::Duration, TemporalValue::Duration(record))
    }

    /// `Temporal.Instant.prototype.round`: `roundTo` is a unit or an options
    /// object with a required `smallestUnit`.
    fn instant_round(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let nanos = self.instant_of(receiver)?;
        let round_to = argument(arguments, 0);
        let (increment, mode, smallest) = match &round_to {
            JsValue::Undefined => {
                return Err(JsError::type_error("round requires an options argument"));
            }
            JsValue::String(text) => {
                let Some(unit) = Unit::parse(text) else {
                    return Err(self.range_error("invalid smallestUnit"));
                };
                (1, RoundingMode::HalfExpand, Some(unit))
            }
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
        let unit_nanos = unit.nanos().unwrap_or(1);
        let rounded = round_to_increment(nanos, unit_nanos * increment, mode);
        self.instant_value(rounded)
    }

    /// `toString` and `toJSON`: the options set the precision and rounding.
    /// `timeZone` may only name UTC, since no other zone is resolved here.
    fn instant_to_string(
        &mut self,
        dom: &mut Dom,
        nanos: i128,
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
        let zone = self.option_value(dom, options, "timeZone")?;
        if !matches!(zone, JsValue::Undefined) {
            let text = self.to_string_checked(dom, &zone)?;
            if !text.eq_ignore_ascii_case("UTC") {
                return Err(self.range_error("only the UTC time zone is supported"));
            }
        }
        let (precision, unit_nanos) = seconds_precision(smallest, digits);
        let rounded = round_to_increment(nanos, unit_nanos, mode);
        if !instant_within_limits(rounded) {
            return Err(self.range_error("Temporal.Instant is outside the representable range"));
        }
        Ok(instant_text(rounded, precision))
    }

    /// `Temporal.Instant.compare`, returning -1, 0 or 1.
    fn instant_compare(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let one = self.to_temporal_instant(dom, &argument(arguments, 0))?;
        let two = self.to_temporal_instant(dom, &argument(arguments, 1))?;
        Ok(JsValue::Number(match one.cmp(&two) {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        }))
    }

    /// The Instant statics, getters and methods, dispatched by name.
    pub(super) fn instant_native(
        &mut self,
        dom: &mut Dom,
        method: TemporalMethod,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let first = argument(arguments, 0);
        match method {
            TemporalMethod::From => {
                let nanos = self.to_temporal_instant(dom, &first)?;
                self.instant_value(nanos)
            }
            TemporalMethod::FromEpochMilliseconds => {
                let number = self.to_number_value(dom, &first)?;
                if !number.is_finite() || number.trunc() != number {
                    return Err(self.range_error("epoch milliseconds must be an integer"));
                }
                let nanos = number as i128 * NANOS_PER_MILLI;
                self.instant_value(nanos)
            }
            TemporalMethod::FromEpochNanoseconds => {
                let value = self.to_bigint_value(dom, &first)?;
                let Some(nanos) = nanos_of_bigint(&value) else {
                    return Err(self.range_error("epoch nanoseconds are out of range"));
                };
                self.instant_value(nanos)
            }
            TemporalMethod::Compare => self.instant_compare(dom, arguments),
            TemporalMethod::EpochMilliseconds => {
                let nanos = self.instant_of(receiver)?;
                Ok(JsValue::Number(nanos.div_euclid(NANOS_PER_MILLI) as f64))
            }
            TemporalMethod::EpochNanoseconds => {
                let nanos = self.instant_of(receiver)?;
                Ok(JsValue::BigInt(bigint_of(nanos)))
            }
            TemporalMethod::Add | TemporalMethod::Subtract => {
                let nanos = self.instant_of(receiver)?;
                let sign = if method == TemporalMethod::Add { 1 } else { -1 };
                self.instant_add(dom, nanos, &first, sign)
            }
            TemporalMethod::Until => self.instant_difference(dom, receiver, arguments, false),
            TemporalMethod::Since => self.instant_difference(dom, receiver, arguments, true),
            TemporalMethod::Round => self.instant_round(dom, receiver, arguments),
            TemporalMethod::Equals => {
                let nanos = self.instant_of(receiver)?;
                let other = self.to_temporal_instant(dom, &first)?;
                Ok(JsValue::Boolean(nanos == other))
            }
            TemporalMethod::ToString => {
                let nanos = self.instant_of(receiver)?;
                let text = self.instant_to_string(dom, nanos, &first)?;
                Ok(JsValue::String(text))
            }
            // `toJSON` and `toLocaleString` take no options, so none are read.
            TemporalMethod::ToJSON | TemporalMethod::ToLocaleString => {
                let nanos = self.instant_of(receiver)?;
                let text = self.instant_to_string(dom, nanos, &JsValue::Undefined)?;
                Ok(JsValue::String(text))
            }
            TemporalMethod::ValueOf => Err(JsError::type_error(
                "Temporal.Instant cannot be converted to a primitive; use compare() or toString()",
            )),
            _ => Err(JsError::type_error("unsupported Temporal.Instant member")),
        }
    }
}

/// The members of `Temporal.Instant` and its prototype.
pub(super) const SURFACE: Surface = Surface {
    constructor_length: Some(1),
    statics: &[
        member("from", TemporalMethod::From, 1),
        member(
            "fromEpochMilliseconds",
            TemporalMethod::FromEpochMilliseconds,
            1,
        ),
        member(
            "fromEpochNanoseconds",
            TemporalMethod::FromEpochNanoseconds,
            1,
        ),
        member("compare", TemporalMethod::Compare, 2),
    ],
    methods: &[
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
        ("epochMilliseconds", TemporalMethod::EpochMilliseconds),
        ("epochNanoseconds", TemporalMethod::EpochNanoseconds),
    ],
};
