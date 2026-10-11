//! Reading option bags and typed arguments the way the Temporal specification
//! reads them: each `Get` happens in the specified order, each conversion
//! runs user code exactly where the spec does, and each refusal is the error
//! kind the spec names (`TypeError` for a wrong type, `RangeError` for a bad
//! value).

use super::TemporalValue;
use super::rounding::{RoundingMode, Unit, increment_is_valid};
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::eval::PrimitiveHint;
use render_dom::Dom;

/// The `calendarName` option of the `toString` methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CalendarDisplay {
    Auto,
    Always,
    Never,
    Critical,
}

impl JsRuntime {
    /// `ToString` for a value the Temporal algorithms read: a Symbol is a
    /// `TypeError`, where the engine's own conversion would stringify it.
    pub(super) fn to_string_checked(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<String, JsError> {
        match self.to_primitive_with_hint(dom, value.clone(), PrimitiveHint::String)? {
            JsValue::Symbol(_) => Err(JsError::type_error(
                "Cannot convert a Symbol value to a string",
            )),
            primitive => Ok(primitive.to_js_string()),
        }
    }

    /// `GetOptionsObject`: `undefined` is an empty bag, an object is read,
    /// and anything else is a `TypeError`.
    pub(super) fn options_object(value: &JsValue) -> Result<Option<ObjectId>, JsError> {
        match value {
            JsValue::Undefined => Ok(None),
            JsValue::Object(object) => Ok(Some(*object)),
            _ => Err(JsError::type_error(
                "options must be an object or undefined",
            )),
        }
    }

    /// `Get(options, name)`, or `undefined` when there is no bag.
    pub(super) fn option_value(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
        name: &str,
    ) -> Result<JsValue, JsError> {
        match options {
            Some(object) => self.get_member(dom, object, name),
            None => Ok(JsValue::Undefined),
        }
    }

    /// `GetOption` for a string option: the value must be one of `allowed`.
    /// `None` when the option is absent, so the caller applies its default.
    pub(super) fn enumerated_option(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
        name: &str,
        allowed: &[&'static str],
    ) -> Result<Option<&'static str>, JsError> {
        let value = self.option_value(dom, options, name)?;
        if matches!(value, JsValue::Undefined) {
            return Ok(None);
        }
        let text = self.to_string_checked(dom, &value)?;
        allowed
            .iter()
            .copied()
            .find(|candidate| *candidate == text)
            .map(Some)
            .ok_or_else(|| self.option_range_error(name, &text))
    }

    /// `overflow`: `true` for `constrain`, `false` for `reject`.
    pub(super) fn overflow_option(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
    ) -> Result<bool, JsError> {
        Ok(self
            .enumerated_option(dom, options, "overflow", &["constrain", "reject"])?
            .is_none_or(|choice| choice == "constrain"))
    }

    /// `roundingMode`, read with `fallback` when absent.
    pub(super) fn rounding_mode_option(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
        fallback: RoundingMode,
    ) -> Result<RoundingMode, JsError> {
        let choice = self.enumerated_option(
            dom,
            options,
            "roundingMode",
            &[
                "ceil",
                "floor",
                "expand",
                "trunc",
                "halfCeil",
                "halfFloor",
                "halfExpand",
                "halfTrunc",
                "halfEven",
            ],
        )?;
        Ok(choice.and_then(RoundingMode::parse).unwrap_or(fallback))
    }

    /// `roundingIncrement`: an integer in 1 through 10^9, defaulting to 1.
    pub(super) fn rounding_increment_option(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
    ) -> Result<i128, JsError> {
        let value = self.option_value(dom, options, "roundingIncrement")?;
        if matches!(value, JsValue::Undefined) {
            return Ok(1);
        }
        let integer = self.to_integer_with_truncation(dom, &value)?;
        if !(1.0..=1_000_000_000.0).contains(&integer) {
            return Err(self.range_error("roundingIncrement must be between 1 and 10^9"));
        }
        Ok(integer as i128)
    }

    /// A unit-valued option. `None` when the option is absent, or when it is
    /// `auto` and `allow_auto` is set; any other unknown name is a `RangeError`.
    pub(super) fn unit_option(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
        name: &str,
        allow_auto: bool,
    ) -> Result<Option<Unit>, JsError> {
        let value = self.option_value(dom, options, name)?;
        if matches!(value, JsValue::Undefined) {
            return Ok(None);
        }
        let text = self.to_string_checked(dom, &value)?;
        if allow_auto && text == "auto" {
            return Ok(None);
        }
        Unit::parse(&text)
            .map(Some)
            .ok_or_else(|| self.option_range_error(name, &text))
    }

    /// `fractionalSecondDigits`: `None` for `auto` or an absent option,
    /// otherwise the number of digits, 0 through 9.
    pub(super) fn fractional_digits_option(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
    ) -> Result<Option<u8>, JsError> {
        let value = self.option_value(dom, options, "fractionalSecondDigits")?;
        let number = match value {
            JsValue::Undefined => return Ok(None),
            JsValue::Number(number) => number,
            other => {
                let text = self.to_string_checked(dom, &other)?;
                if text != "auto" {
                    return Err(self.range_error(
                        "fractionalSecondDigits must be 'auto' or a number from 0 to 9",
                    ));
                }
                return Ok(None);
            }
        };
        if !number.is_finite() {
            return Err(self.range_error("fractionalSecondDigits must be a finite number"));
        }
        let digits = number.floor();
        if !(0.0..=9.0).contains(&digits) {
            return Err(self.range_error("fractionalSecondDigits must be from 0 to 9"));
        }
        Ok(Some(digits as u8))
    }

    /// `calendarName`, which `toString` reads for every class with a calendar.
    pub(super) fn calendar_name_option(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
    ) -> Result<CalendarDisplay, JsError> {
        Ok(
            match self.enumerated_option(
                dom,
                options,
                "calendarName",
                &["auto", "always", "never", "critical"],
            )? {
                Some("always") => CalendarDisplay::Always,
                Some("never") => CalendarDisplay::Never,
                Some("critical") => CalendarDisplay::Critical,
                _ => CalendarDisplay::Auto,
            },
        )
    }

    /// `ToTemporalCalendarIdentifier` for the ISO calendar: `undefined` is
    /// the default, a Temporal object carries its own, and a string names
    /// `iso8601` or is an ISO string whose calendar annotation does. Any other
    /// calendar, and any other value, is refused.
    pub(super) fn calendar_id(&mut self, value: &JsValue) -> Result<(), JsError> {
        match value {
            JsValue::Undefined => Ok(()),
            JsValue::Object(object)
                if matches!(
                    self.temporal_host(*object),
                    Some(
                        TemporalValue::PlainDate(_)
                            | TemporalValue::PlainDateTime(_)
                            | TemporalValue::PlainMonthDay(_)
                            | TemporalValue::PlainYearMonth(_)
                    )
                ) =>
            {
                Ok(())
            }
            JsValue::Object(_) => Err(JsError::type_error(
                "calendar must be a string or a Temporal object",
            )),
            JsValue::String(text) => {
                let named = if text.eq_ignore_ascii_case("iso8601") {
                    None
                } else {
                    super::parse::parse_date_time(text).map(|parsed| parsed.calendar)
                };
                match named {
                    None if text.eq_ignore_ascii_case("iso8601") => Ok(()),
                    Some(None) => Ok(()),
                    Some(Some(calendar)) if calendar.eq_ignore_ascii_case("iso8601") => Ok(()),
                    _ => Err(self.range_error("unsupported calendar; only iso8601 is available")),
                }
            }
            _ => Err(JsError::type_error(
                "calendar must be a string or a Temporal object",
            )),
        }
    }

    /// `ToIntegerWithTruncation`: `ToNumber`, refusing NaN and the infinities,
    /// then truncating toward zero.
    pub(super) fn to_integer_with_truncation(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<f64, JsError> {
        let number = self.to_number_value(dom, value)?;
        if !number.is_finite() {
            return Err(self.range_error("expected a finite number"));
        }
        Ok(number.trunc() + 0.0)
    }

    /// `ToIntegerIfIntegral`: a finite number with no fractional part.
    pub(super) fn to_integer_if_integral(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<f64, JsError> {
        let number = self.to_number_value(dom, value)?;
        if !number.is_finite() || number.trunc() != number {
            return Err(self.range_error("expected an integer"));
        }
        Ok(number + 0.0)
    }

    /// The `RangeError` for an option value outside its allowed set.
    pub(super) fn option_range_error(&mut self, name: &str, value: &str) -> JsError {
        self.range_error(&format!("invalid value {value:?} for option {name:?}"))
    }

    /// `GetTemporalUnitValuedOption` restricted to `group`: a unit outside the
    /// group is a `RangeError`, as is `auto` when `allow_auto` is not set.
    pub(super) fn unit_in_group(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
        name: &str,
        group: UnitGroup,
        allow_auto: bool,
    ) -> Result<Option<Unit>, JsError> {
        let unit = self.unit_option(dom, options, name, allow_auto)?;
        match unit {
            Some(unit) if !group.contains(unit) => {
                Err(self.option_range_error(name, unit.singular()))
            }
            other => Ok(other),
        }
    }

    /// `GetDifferenceSettings`: the largest and smallest units, the increment
    /// and the rounding mode of a `since` or `until` (`since` negates the mode).
    /// Reads `largestUnit`, `roundingIncrement`, `roundingMode`, then
    /// `smallestUnit`, as the specification orders them.
    pub(super) fn difference_settings(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
        since: bool,
        group: UnitGroup,
        fallback_smallest: Unit,
        default_largest_floor: Unit,
    ) -> Result<DifferenceSettings, JsError> {
        // Every option is read before any of them is validated against the group.
        let largest = self.unit_option(dom, options, "largestUnit", true)?;
        let increment = self.rounding_increment_option(dom, options)?;
        // A difference rounds toward zero unless told otherwise.
        let mode = self.rounding_mode_option(dom, options, RoundingMode::Trunc)?;
        let mode = if since { mode.negated() } else { mode };
        let smallest = self.unit_option(dom, options, "smallestUnit", false)?;
        for unit in [largest, smallest].into_iter().flatten() {
            if !group.contains(unit) {
                return Err(self.option_range_error("unit", unit.singular()));
            }
        }
        let smallest = smallest.unwrap_or(fallback_smallest);
        let default_largest = default_largest_floor.min(smallest);
        let largest = largest.unwrap_or(default_largest);
        if largest > smallest {
            return Err(self.range_error("largestUnit must not be smaller than smallestUnit"));
        }
        if !increment_is_valid(smallest, increment) {
            return Err(self.range_error("roundingIncrement is not valid for this unit"));
        }
        Ok(DifferenceSettings {
            largest,
            increment,
            mode,
            smallest,
        })
    }
}

/// The units an option may name for one class: the calendar units (`Date`),
/// the clock units (`Time`), or every unit (`DateTime`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnitGroup {
    Date,
    Time,
    /// A day and the time units: what `PlainDateTime.round` accepts.
    DayAndTime,
    /// The time units below an hour: what the `toString` methods accept.
    MinuteAndFiner,
    DateTime,
}

impl UnitGroup {
    fn contains(self, unit: Unit) -> bool {
        match self {
            Self::Date => matches!(unit, Unit::Year | Unit::Month | Unit::Week | Unit::Day),
            Self::Time => unit.is_time(),
            Self::DayAndTime => unit == Unit::Day || unit.is_time(),
            Self::MinuteAndFiner => unit.is_time() && unit != Unit::Hour,
            Self::DateTime => true,
        }
    }
}

/// The resolved `since` and `until` options: the units to measure in, the
/// increment and mode to round with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DifferenceSettings {
    pub(super) largest: Unit,
    pub(super) increment: i128,
    pub(super) mode: RoundingMode,
    pub(super) smallest: Unit,
}
