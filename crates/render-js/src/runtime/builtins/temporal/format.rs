//! The ISO 8601 text that Temporal's `toString` methods produce. Every value
//! passed in is already rounded to the requested precision, so this module
//! only renders digits.

use std::fmt::Write as _;

use super::iso::{IsoDate, IsoTime, NANOS_PER_SECOND};
use super::options::CalendarDisplay;

/// How many fractional-second digits a time or duration prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Precision {
    /// As many digits as are non-zero, with no trailing zeros.
    Auto,
    /// Whole minutes only: no seconds field.
    Minute,
    /// A fixed number of fractional digits, from 0 to 9.
    Digits(u8),
}

/// The ISO year: four digits in 0 through 9999, otherwise a sign and six digits.
pub(crate) fn year_text(year: i32) -> String {
    if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!(
            "{}{:06}",
            if year < 0 { '-' } else { '+' },
            year.unsigned_abs()
        )
    }
}

/// `YYYY-MM-DD`.
pub(crate) fn date_text(date: IsoDate) -> String {
    format!("{}-{:02}-{:02}", year_text(date.year), date.month, date.day)
}

/// `HH:MM`, then `:SS` and the fraction as `precision` asks.
pub(crate) fn time_text(time: IsoTime, precision: Precision) -> String {
    let mut text = format!("{:02}:{:02}", time.hour, time.minute);
    if precision == Precision::Minute {
        return text;
    }
    let sub_second = u64::from(time.millisecond) * 1_000_000
        + u64::from(time.microsecond) * 1_000
        + u64::from(time.nanosecond);
    let _ = write!(text, ":{:02}", time.second);
    text.push_str(&fraction_text(sub_second, precision));
    text
}

/// The `.fraction` that follows a seconds field, or nothing. `sub_second` is
/// in nanoseconds, from 0 through 999,999,999.
pub(crate) fn fraction_text(sub_second: u64, precision: Precision) -> String {
    let digits = format!("{sub_second:09}");
    match precision {
        Precision::Minute | Precision::Digits(0) => String::new(),
        Precision::Digits(count) => format!(".{}", &digits[..usize::from(count)]),
        Precision::Auto => {
            let trimmed = digits.trim_end_matches('0');
            if trimmed.is_empty() {
                String::new()
            } else {
                format!(".{trimmed}")
            }
        }
    }
}

/// The bracketed calendar annotation for `display`. Every Temporal value here
/// uses the ISO calendar, so `auto` and `never` print nothing.
pub(crate) const fn calendar_text(display: CalendarDisplay) -> &'static str {
    match display {
        CalendarDisplay::Auto | CalendarDisplay::Never => "",
        CalendarDisplay::Always => "[u-ca=iso8601]",
        CalendarDisplay::Critical => "[!u-ca=iso8601]",
    }
}

/// `YYYY-MM-DDTHH:MM[:SS[.fraction]]Z`, the `toString` of an instant read in UTC.
pub(crate) fn instant_text(epoch_nanos: i128, precision: Precision) -> String {
    let datetime = super::iso::datetime_from_epoch_nanos(epoch_nanos);
    format!(
        "{}T{}Z",
        date_text(datetime.date),
        time_text(datetime.time, precision)
    )
}

/// The ISO 8601 duration string of a set of Duration fields, with the sign
/// taken from `negative`. Each field is its absolute value.
pub(crate) fn duration_text(
    fields: [u128; 10],
    negative: bool,
    seconds_precision: Precision,
) -> String {
    let [
        years,
        months,
        weeks,
        days,
        hours,
        minutes,
        seconds,
        milliseconds,
        microseconds,
        nanoseconds,
    ] = fields;
    let sign = if negative { "-" } else { "" };
    let mut date_part = String::new();
    for (value, designator) in [(years, 'Y'), (months, 'M'), (weeks, 'W'), (days, 'D')] {
        if value != 0 {
            let _ = write!(date_part, "{value}{designator}");
        }
    }
    let sub_second = seconds * u128::try_from(NANOS_PER_SECOND).unwrap_or(0)
        + milliseconds * 1_000_000
        + microseconds * 1_000
        + nanoseconds;
    let whole_seconds = sub_second / 1_000_000_000;
    let fraction_nanos = (sub_second % 1_000_000_000) as u64;
    let mut time_part = String::new();
    if hours != 0 {
        let _ = write!(time_part, "{hours}H");
    }
    if minutes != 0 {
        let _ = write!(time_part, "{minutes}M");
    }
    let show_seconds = seconds_precision != Precision::Minute
        && (whole_seconds != 0 || fraction_nanos != 0 || seconds_precision != Precision::Auto);
    if show_seconds {
        let fraction = fraction_text(fraction_nanos, seconds_precision);
        let _ = write!(time_part, "{whole_seconds}{fraction}S");
    }
    if date_part.is_empty() && time_part.is_empty() {
        return "PT0S".to_owned();
    }
    let mut text = format!("{sign}P{date_part}");
    if !time_part.is_empty() {
        text.push('T');
        text.push_str(&time_part);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::builtins::temporal::iso::MIDNIGHT;

    #[test]
    fn years_outside_four_digits_take_a_sign_and_six_digits() {
        assert_eq!(year_text(2020), "2020");
        assert_eq!(year_text(0), "0000");
        assert_eq!(year_text(-1), "-000001");
        assert_eq!(year_text(10_000), "+010000");
    }

    #[test]
    fn auto_precision_trims_trailing_zeros_and_fixed_precision_pads() {
        let time = IsoTime {
            hour: 1,
            minute: 2,
            second: 3,
            millisecond: 500,
            microsecond: 0,
            nanosecond: 0,
        };
        assert_eq!(time_text(time, Precision::Auto), "01:02:03.5");
        assert_eq!(time_text(time, Precision::Digits(3)), "01:02:03.500");
        assert_eq!(time_text(time, Precision::Minute), "01:02");
        assert_eq!(time_text(MIDNIGHT, Precision::Auto), "00:00:00");
    }

    #[test]
    fn duration_text_follows_the_designator_and_zero_rules() {
        assert_eq!(duration_text([0; 10], false, Precision::Auto), "PT0S");
        assert_eq!(
            duration_text([1, 0, 0, 2, 3, 0, 0, 0, 0, 0], true, Precision::Auto),
            "-P1Y2DT3H"
        );
        assert_eq!(
            duration_text([0, 0, 0, 0, 0, 0, 1, 500, 0, 0], false, Precision::Auto),
            "PT1.5S"
        );
        assert_eq!(
            duration_text([0, 0, 0, 0, 0, 0, 0, 0, 0, 0], false, Precision::Digits(2)),
            "PT0.00S"
        );
    }
}
