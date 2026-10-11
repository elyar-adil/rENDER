//! Temporal units and rounding, on exact integers. A quantity is an `i128`
//! count of the unit being rounded, or of nanoseconds when the unit is a
//! time unit, so no step here loses precision.

use super::iso::NANOS_PER_DAY;

/// A Temporal unit, from the largest (`Year`) to the smallest (`Nanosecond`).
/// The derived order is that size order, so `min` picks the larger unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Unit {
    Year,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
}

impl Unit {
    /// The singular name, as `smallestUnit` and `largestUnit` spell it.
    pub(crate) const fn singular(self) -> &'static str {
        match self {
            Self::Year => "year",
            Self::Month => "month",
            Self::Week => "week",
            Self::Day => "day",
            Self::Hour => "hour",
            Self::Minute => "minute",
            Self::Second => "second",
            Self::Millisecond => "millisecond",
            Self::Microsecond => "microsecond",
            Self::Nanosecond => "nanosecond",
        }
    }

    /// The unit a singular or plural name denotes (`ParseTemporalUnit`).
    pub(crate) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "year" | "years" => Self::Year,
            "month" | "months" => Self::Month,
            "week" | "weeks" => Self::Week,
            "day" | "days" => Self::Day,
            "hour" | "hours" => Self::Hour,
            "minute" | "minutes" => Self::Minute,
            "second" | "seconds" => Self::Second,
            "millisecond" | "milliseconds" => Self::Millisecond,
            "microsecond" | "microseconds" => Self::Microsecond,
            "nanosecond" | "nanoseconds" => Self::Nanosecond,
            _ => return None,
        })
    }

    /// The length of one of this unit in nanoseconds, for the units whose
    /// length is fixed. Years, months and weeks depend on the date they are
    /// measured from, so they have none.
    pub(crate) const fn nanos(self) -> Option<i128> {
        match self {
            Self::Day => Some(NANOS_PER_DAY),
            Self::Hour => Some(3_600_000_000_000),
            Self::Minute => Some(60_000_000_000),
            Self::Second => Some(1_000_000_000),
            Self::Millisecond => Some(1_000_000),
            Self::Microsecond => Some(1_000),
            Self::Nanosecond => Some(1),
            Self::Year | Self::Month | Self::Week => None,
        }
    }

    /// Whether the unit is one of the time units (hours and smaller).
    pub(crate) const fn is_time(self) -> bool {
        matches!(
            self,
            Self::Hour
                | Self::Minute
                | Self::Second
                | Self::Millisecond
                | Self::Microsecond
                | Self::Nanosecond
        )
    }
}

/// The rounding modes of `roundingMode`, named as the specification names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoundingMode {
    Ceil,
    Floor,
    Expand,
    Trunc,
    HalfCeil,
    HalfFloor,
    HalfExpand,
    HalfTrunc,
    HalfEven,
}

impl RoundingMode {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "ceil" => Self::Ceil,
            "floor" => Self::Floor,
            "expand" => Self::Expand,
            "trunc" => Self::Trunc,
            "halfCeil" => Self::HalfCeil,
            "halfFloor" => Self::HalfFloor,
            "halfExpand" => Self::HalfExpand,
            "halfTrunc" => Self::HalfTrunc,
            "halfEven" => Self::HalfEven,
            _ => return None,
        })
    }

    /// `NegateRoundingMode`: `since` rounds the mirror image of `until`.
    pub(crate) const fn negated(self) -> Self {
        match self {
            Self::Ceil => Self::Floor,
            Self::Floor => Self::Ceil,
            Self::HalfCeil => Self::HalfFloor,
            Self::HalfFloor => Self::HalfCeil,
            other => other,
        }
    }
}

/// `ValidateTemporalRoundingIncrement` for `unit`: the increment must divide
/// the unit's maximum and be below it (or at most it, for days). The calendar
/// units have no maximum, so any increment is accepted for them.
pub(crate) fn increment_is_valid(unit: Unit, increment: i128) -> bool {
    let maximum = match unit {
        Unit::Year | Unit::Month | Unit::Week => return true,
        Unit::Day => 1_000_000_000,
        Unit::Hour => 24,
        Unit::Minute | Unit::Second => 60,
        Unit::Millisecond | Unit::Microsecond | Unit::Nanosecond => 1_000,
    };
    let within = if unit == Unit::Day {
        increment <= maximum
    } else {
        increment < maximum
    };
    within && maximum % increment == 0
}

/// `RoundNumberToIncrement` on an exact integer: `value` rounded to a
/// multiple of `increment` (which must be positive) under `mode`.
pub(crate) fn round_to_increment(value: i128, increment: i128, mode: RoundingMode) -> i128 {
    let quotient = value.div_euclid(increment);
    let remainder = value.rem_euclid(increment);
    if remainder == 0 {
        return value;
    }
    let positive = value > 0;
    let up = match mode {
        RoundingMode::Ceil => true,
        RoundingMode::Floor => false,
        RoundingMode::Expand => positive,
        RoundingMode::Trunc => !positive,
        RoundingMode::HalfCeil
        | RoundingMode::HalfFloor
        | RoundingMode::HalfExpand
        | RoundingMode::HalfTrunc
        | RoundingMode::HalfEven => {
            let twice = remainder * 2;
            match twice.cmp(&increment) {
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Equal => match mode {
                    RoundingMode::HalfCeil => true,
                    RoundingMode::HalfFloor => false,
                    RoundingMode::HalfExpand => positive,
                    RoundingMode::HalfTrunc => !positive,
                    _ => quotient.rem_euclid(2) == 1,
                },
            }
        }
    };
    (quotient + i128::from(up)) * increment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_rounds_a_positive_and_a_negative_tie_as_specified() {
        let cases = [
            (RoundingMode::Ceil, 25, 30, -25, -20),
            (RoundingMode::Floor, 25, 20, -25, -30),
            (RoundingMode::Expand, 25, 30, -25, -30),
            (RoundingMode::Trunc, 25, 20, -25, -20),
            (RoundingMode::HalfCeil, 25, 30, -25, -20),
            (RoundingMode::HalfFloor, 25, 20, -25, -30),
            (RoundingMode::HalfExpand, 25, 30, -25, -30),
            (RoundingMode::HalfTrunc, 25, 20, -25, -20),
            (RoundingMode::HalfEven, 25, 20, -35, -40),
        ];
        for (mode, positive, positive_expected, negative, negative_expected) in cases {
            assert_eq!(
                round_to_increment(positive, 10, mode),
                positive_expected,
                "{mode:?} positive tie"
            );
            assert_eq!(
                round_to_increment(negative, 10, mode),
                negative_expected,
                "{mode:?} negative tie"
            );
        }
        assert_eq!(round_to_increment(21, 10, RoundingMode::Ceil), 30);
        assert_eq!(round_to_increment(20, 10, RoundingMode::Ceil), 20);
    }

    #[test]
    fn unit_names_and_order_follow_the_specification() {
        assert_eq!(Unit::parse("days"), Some(Unit::Day));
        assert_eq!(Unit::parse("day"), Some(Unit::Day));
        assert_eq!(Unit::parse("Day"), None);
        assert!(Unit::Year < Unit::Nanosecond);
        assert_eq!(Unit::Hour.nanos(), Some(3_600_000_000_000));
        assert_eq!(Unit::Week.nanos(), None);
        assert_eq!(RoundingMode::HalfCeil.negated(), RoundingMode::HalfFloor);
        assert_eq!(RoundingMode::Expand.negated(), RoundingMode::Expand);
    }
}
