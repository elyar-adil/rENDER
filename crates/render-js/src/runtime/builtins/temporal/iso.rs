//! Pure ISO 8601 calendar and clock arithmetic for Temporal. Nothing here
//! touches a JavaScript value: fields arrive already converted, and a
//! refusal is `None`, which the caller turns into a `RangeError`.
//!
//! Epoch days count from 1970-01-01 and epoch nanoseconds from the same
//! instant. Both fit `i64` and `i128` with room to spare for every value the
//! Temporal limits allow.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    reason = "calendar fields are small and checked at each conversion"
)]

pub(crate) const NANOS_PER_SECOND: i128 = 1_000_000_000;
pub(crate) const NANOS_PER_DAY: i128 = 86_400 * NANOS_PER_SECOND;
/// The largest magnitude, in nanoseconds, of an `Instant` (ECMA-262-style
/// `IsValidEpochNanoseconds` bound of Temporal): 10^8 days.
pub(crate) const MAX_INSTANT_NANOS: i128 = 100_000_000 * NANOS_PER_DAY;

/// A calendar date of the ISO 8601 proleptic Gregorian calendar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct IsoDate {
    pub(crate) year: i32,
    pub(crate) month: u8,
    pub(crate) day: u8,
}

/// A wall-clock time of day, down to the nanosecond.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct IsoTime {
    pub(crate) hour: u8,
    pub(crate) minute: u8,
    pub(crate) second: u8,
    pub(crate) millisecond: u16,
    pub(crate) microsecond: u16,
    pub(crate) nanosecond: u16,
}

/// A date and a time of day, with no zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct IsoDateTime {
    pub(crate) date: IsoDate,
    pub(crate) time: IsoTime,
}

pub(crate) const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub(crate) const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap_year(year) => 29,
        _ => 28,
    }
}

pub(crate) const fn days_in_year(year: i32) -> u16 {
    if is_leap_year(year) { 366 } else { 365 }
}

/// Days from 1970-01-01 to `date`, negative before the epoch.
pub(crate) fn epoch_days(date: IsoDate) -> i64 {
    let year = i64::from(date.year) - i64::from(date.month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = (i64::from(date.month) + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(date.day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date `days` after 1970-01-01.
pub(crate) fn date_from_epoch_days(days: i64) -> IsoDate {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    IsoDate {
        year: year as i32,
        month: month as u8,
        day: day as u8,
    }
}

/// `date` moved by `days` calendar days, with no limit check.
pub(crate) fn add_days(date: IsoDate, days: i64) -> IsoDate {
    date_from_epoch_days(epoch_days(date) + days)
}

/// Day of the week, 1 for Monday through 7 for Sunday.
pub(crate) fn day_of_week(date: IsoDate) -> u8 {
    ((epoch_days(date) + 3).rem_euclid(7) + 1) as u8
}

/// Day of the year, 1 for January 1st.
pub(crate) fn day_of_year(date: IsoDate) -> u16 {
    let january_first = IsoDate {
        year: date.year,
        month: 1,
        day: 1,
    };
    (epoch_days(date) - epoch_days(january_first) + 1) as u16
}

/// The ISO week-numbering year and week of `date`, where week 1 is the week
/// containing the year's first Thursday.
pub(crate) fn iso_week(date: IsoDate) -> (i32, u8) {
    let thursday = date_from_epoch_days(epoch_days(date) - i64::from(day_of_week(date)) + 4);
    let january_first = IsoDate {
        year: thursday.year,
        month: 1,
        day: 1,
    };
    let week = (epoch_days(thursday) - epoch_days(january_first)) / 7 + 1;
    (thursday.year, week as u8)
}

/// Whether `year`, `month` and `day` name a day of the calendar.
pub(crate) fn is_valid_date(year: i32, month: i64, day: i64) -> bool {
    (1..=12).contains(&month) && day >= 1 && day <= i64::from(days_in_month(year, month as u8))
}

/// `RegulateISODate`: under `constrain` the month and day are clamped into
/// range; otherwise an out-of-range field is `None`.
pub(crate) fn regulate_date(year: i32, month: i64, day: i64, constrain: bool) -> Option<IsoDate> {
    if constrain {
        let month = month.clamp(1, 12);
        let day = day.clamp(1, i64::from(days_in_month(year, month as u8)));
        return Some(IsoDate {
            year,
            month: month as u8,
            day: day as u8,
        });
    }
    is_valid_date(year, month, day).then_some(IsoDate {
        year,
        month: month as u8,
        day: day as u8,
    })
}

/// `RegulateTime`: under `constrain` each field is clamped to its range,
/// otherwise an out-of-range field is `None`. Fields arrive as integers.
pub(crate) fn regulate_time(fields: [i64; 6], constrain: bool) -> Option<IsoTime> {
    let limits = [23, 59, 59, 999, 999, 999];
    let mut values = [0_i64; 6];
    for (index, (value, limit)) in fields.iter().zip(limits).enumerate() {
        values[index] = if constrain {
            (*value).clamp(0, limit)
        } else if (0..=limit).contains(value) {
            *value
        } else {
            return None;
        };
    }
    Some(IsoTime {
        hour: values[0] as u8,
        minute: values[1] as u8,
        second: values[2] as u8,
        millisecond: values[3] as u16,
        microsecond: values[4] as u16,
        nanosecond: values[5] as u16,
    })
}

/// Whether a date is inside the range `PlainDate` represents: from
/// `-271821-04-19` (one day before the instant range) through `+275760-09-13`.
pub(crate) fn date_within_limits(date: IsoDate) -> bool {
    (-100_000_001..=100_000_000).contains(&epoch_days(date))
}

pub(crate) fn time_nanos(time: IsoTime) -> i128 {
    i128::from(time.hour) * 3_600 * NANOS_PER_SECOND
        + i128::from(time.minute) * 60 * NANOS_PER_SECOND
        + i128::from(time.second) * NANOS_PER_SECOND
        + i128::from(time.millisecond) * 1_000_000
        + i128::from(time.microsecond) * 1_000
        + i128::from(time.nanosecond)
}

/// The time of day `nanos` into a day, for `0 <= nanos < NANOS_PER_DAY`.
pub(crate) fn time_from_nanos(nanos: i128) -> IsoTime {
    let mut rest = nanos;
    let hour = rest / (3_600 * NANOS_PER_SECOND);
    rest %= 3_600 * NANOS_PER_SECOND;
    let minute = rest / (60 * NANOS_PER_SECOND);
    rest %= 60 * NANOS_PER_SECOND;
    let second = rest / NANOS_PER_SECOND;
    rest %= NANOS_PER_SECOND;
    let millisecond = rest / 1_000_000;
    rest %= 1_000_000;
    let microsecond = rest / 1_000;
    let nanosecond = rest % 1_000;
    IsoTime {
        hour: hour as u8,
        minute: minute as u8,
        second: second as u8,
        millisecond: millisecond as u16,
        microsecond: microsecond as u16,
        nanosecond: nanosecond as u16,
    }
}

pub(crate) const MIDNIGHT: IsoTime = IsoTime {
    hour: 0,
    minute: 0,
    second: 0,
    millisecond: 0,
    microsecond: 0,
    nanosecond: 0,
};

/// `GetUTCEpochNanoseconds`: the instant at which `datetime` reads as UTC.
pub(crate) fn datetime_epoch_nanos(datetime: IsoDateTime) -> i128 {
    i128::from(epoch_days(datetime.date)) * NANOS_PER_DAY + time_nanos(datetime.time)
}

pub(crate) fn datetime_from_epoch_nanos(nanos: i128) -> IsoDateTime {
    let days = nanos.div_euclid(NANOS_PER_DAY);
    let rest = nanos.rem_euclid(NANOS_PER_DAY);
    IsoDateTime {
        date: date_from_epoch_days(days as i64),
        time: time_from_nanos(rest),
    }
}

/// Whether an `Instant` of `nanos` is in range (`IsValidEpochNanoseconds`).
pub(crate) fn instant_within_limits(nanos: i128) -> bool {
    nanos.abs() <= MAX_INSTANT_NANOS
}

/// Whether a date and time are in range for `PlainDateTime`: strictly within
/// 10^8 + 1 days of the epoch, so `-271821-04-19T00:00:00.000000001` and
/// `+275760-09-13T23:59:59.999999999` are in and the day either side is out.
pub(crate) fn datetime_within_limits(datetime: IsoDateTime) -> bool {
    datetime_epoch_nanos(datetime).abs() < MAX_INSTANT_NANOS + NANOS_PER_DAY
}

/// The `monthCode` of a month: `M` and two digits.
pub(crate) fn month_code(month: u8) -> String {
    format!("M{month:02}")
}

/// `AddISODate`: `date` moved by whole years and months (the day constrained
/// or rejected as `constrain` says), then by weeks and days. `None` when the
/// result is outside the representable range or a field was rejected.
pub(crate) fn add_iso_date(
    date: IsoDate,
    years: i64,
    months: i64,
    weeks: i64,
    days: i64,
    constrain: bool,
) -> Option<IsoDate> {
    let month_index = i64::from(date.month) - 1 + months;
    let year = i64::from(date.year) + years + month_index.div_euclid(12);
    if year.abs() > 1_000_000 {
        return None;
    }
    let month = month_index.rem_euclid(12) + 1;
    let regulated = regulate_date(year as i32, month, i64::from(date.day), constrain)?;
    let total_days = weeks.checked_mul(7)?.checked_add(days)?;
    if total_days.abs() > 400_000_000 {
        return None;
    }
    let result = add_days(regulated, total_days);
    date_within_limits(result).then_some(result)
}

/// `DifferenceISODate`: the years, months, weeks and days from `one` to
/// `two`, with `largest` as the largest unit used. Each count has the sign of
/// the difference, and a component that would overshoot is pulled back.
pub(crate) fn difference_iso_date(
    one: IsoDate,
    two: IsoDate,
    largest: super::rounding::Unit,
) -> (i64, i64, i64, i64) {
    use super::rounding::Unit;
    if one == two {
        return (0, 0, 0, 0);
    }
    let sign: i64 = if two > one { 1 } else { -1 };
    let overshoots = |candidate: IsoDate| sign * candidate_order(candidate, two) > 0;
    match largest {
        Unit::Year | Unit::Month => {
            let mut years = 0;
            let mut intermediate = one;
            if largest == Unit::Year {
                years = i64::from(two.year) - i64::from(one.year);
                intermediate = add_iso_date(one, years, 0, 0, 0, true).unwrap_or(one);
                if overshoots(intermediate) {
                    years -= sign;
                    intermediate = add_iso_date(one, years, 0, 0, 0, true).unwrap_or(one);
                }
            }
            let mut months = (i64::from(two.year) - i64::from(intermediate.year)) * 12
                + i64::from(two.month)
                - i64::from(intermediate.month);
            let mut landed =
                add_iso_date(intermediate, 0, months, 0, 0, true).unwrap_or(intermediate);
            if overshoots(landed) {
                months -= sign;
                landed = add_iso_date(intermediate, 0, months, 0, 0, true).unwrap_or(intermediate);
            }
            (years, months, 0, epoch_days(two) - epoch_days(landed))
        }
        Unit::Week => {
            let days = epoch_days(two) - epoch_days(one);
            let weeks = days / 7;
            (0, 0, weeks, days - weeks * 7)
        }
        _ => (0, 0, 0, epoch_days(two) - epoch_days(one)),
    }
}

/// The sign of `candidate` relative to `target`: -1, 0 or 1.
fn candidate_order(candidate: IsoDate, target: IsoDate) -> i64 {
    match candidate.cmp(&target) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u8, day: u8) -> IsoDate {
        IsoDate { year, month, day }
    }

    #[test]
    fn epoch_day_round_trip_covers_the_limits() {
        assert_eq!(epoch_days(date(1970, 1, 1)), 0);
        assert_eq!(epoch_days(date(1969, 12, 31)), -1);
        assert_eq!(date_from_epoch_days(-100_000_000), date(-271_821, 4, 20));
        assert_eq!(date_from_epoch_days(100_000_000), date(275_760, 9, 13));
        assert!(date_within_limits(date(-271_821, 4, 19)));
        assert!(!date_within_limits(date(-271_821, 4, 18)));
        assert_eq!(epoch_days(date(2000, 2, 29)), 11_016);
        assert_eq!(date_from_epoch_days(11_016), date(2000, 2, 29));
    }

    #[test]
    fn weekday_and_iso_week_match_the_calendar() {
        assert_eq!(day_of_week(date(1970, 1, 1)), 4);
        assert_eq!(day_of_week(date(2021, 1, 3)), 7);
        assert_eq!(iso_week(date(2021, 1, 3)), (2020, 53));
        assert_eq!(iso_week(date(2020, 12, 31)), (2020, 53));
        assert_eq!(iso_week(date(2024, 12, 30)), (2025, 1));
    }

    #[test]
    fn constrain_and_reject_disagree_only_on_out_of_range_fields() {
        assert_eq!(regulate_date(2021, 2, 30, true), Some(date(2021, 2, 28)));
        assert_eq!(regulate_date(2021, 2, 30, false), None);
        assert_eq!(regulate_date(2020, 2, 30, true), Some(date(2020, 2, 29)));
        assert_eq!(
            regulate_time([25, 0, 0, 0, 0, 0], true).map(|t| t.hour),
            Some(23)
        );
        assert_eq!(regulate_time([25, 0, 0, 0, 0, 0], false), None);
    }

    #[test]
    fn datetime_nanosecond_conversion_is_exact() {
        let datetime = IsoDateTime {
            date: date(1969, 12, 31),
            time: IsoTime {
                hour: 23,
                minute: 59,
                second: 59,
                millisecond: 999,
                microsecond: 999,
                nanosecond: 999,
            },
        };
        assert_eq!(datetime_epoch_nanos(datetime), -1);
        assert_eq!(datetime_from_epoch_nanos(-1), datetime);
        assert!(!datetime_within_limits(IsoDateTime {
            date: date(-271_821, 4, 19),
            time: MIDNIGHT,
        }));
    }
}
