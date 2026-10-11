//! The ISO 8601 and RFC 9557 string grammar that Temporal reads. Each parser
//! answers `None` for any text outside its grammar, and the caller turns that
//! into the `RangeError` the specification names. A parsed date or time is
//! syntactically well formed but not yet checked against the calendar, so an
//! out-of-range day is for the caller's `overflow` option to decide.

use super::iso::{IsoDate, IsoTime};

/// The fields of a date-time string.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParsedDateTime {
    pub(crate) date: IsoDate,
    /// The time of day, or `None` for a date-only string. A leap second is
    /// already folded to second 59, as the grammar requires.
    pub(crate) time: Option<IsoTime>,
    /// The UTC offset in nanoseconds, from a numeric offset or a `Z`.
    pub(crate) offset: Option<i128>,
    /// Whether the offset was the `Z` designator.
    pub(crate) utc: bool,
    /// The `u-ca` calendar annotation, if one was present.
    pub(crate) calendar: Option<String>,
    /// Whether a time zone annotation (`[Zone/Name]`) was present.
    pub(crate) time_zone: bool,
}

/// The components of a duration string. A component that carries a fraction
/// stores its digits, which are the only fractional part the grammar allows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParsedDuration {
    pub(crate) negative: bool,
    pub(crate) years: Option<Component>,
    pub(crate) months: Option<Component>,
    pub(crate) weeks: Option<Component>,
    pub(crate) days: Option<Component>,
    pub(crate) hours: Option<Component>,
    pub(crate) minutes: Option<Component>,
    pub(crate) seconds: Option<Component>,
}

/// One number of a duration string: its integer part and its fraction digits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Component {
    pub(crate) whole: i128,
    /// The digits after the decimal separator, at most nine of them.
    pub(crate) fraction: String,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            position: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn eat_one_of(&mut self, options: &[u8]) -> bool {
        match self.peek() {
            Some(byte) if options.contains(&byte) => {
                self.position += 1;
                true
            }
            _ => false,
        }
    }

    fn at_end(&self) -> bool {
        self.position >= self.bytes.len()
    }

    /// Exactly `count` decimal digits.
    fn digits(&mut self, count: usize) -> Option<u32> {
        let slice = self.bytes.get(self.position..self.position + count)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.position += count;
        slice
            .iter()
            .try_fold(0_u32, |acc, byte| Some(acc * 10 + u32::from(byte - b'0')))
    }

    /// A run of one or more decimal digits, as text.
    fn digit_run(&mut self) -> Option<&'a str> {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        (self.position > start)
            .then(|| std::str::from_utf8(&self.bytes[start..self.position]).ok())?
    }

    /// The fraction after a decimal separator: one to nine digits.
    fn fraction_digits(&mut self) -> Option<String> {
        let digits = self.digit_run()?;
        (digits.len() <= 9).then(|| digits.to_owned())
    }

    /// The nanoseconds that a fraction of `digits` contributes, padded to nine places.
    fn fraction_nanos(digits: &str) -> u32 {
        let mut value = 0_u32;
        for index in 0..9 {
            let digit = digits.as_bytes().get(index).map_or(0, |byte| byte - b'0');
            value = value * 10 + u32::from(digit);
        }
        value
    }
}

/// `YYYY` (or `+YYYYYY`/`-YYYYYY`) followed by the date's `-MM-DD` or `MMDD`.
/// `-000000` is not a year, and the extended and basic forms do not mix.
fn parse_date(cursor: &mut Cursor<'_>) -> Option<IsoDate> {
    let year = parse_year(cursor)?;
    let extended = cursor.eat_one_of(b"-");
    let month = cursor.digits(2)?;
    if extended && !cursor.eat_one_of(b"-") {
        return None;
    }
    let day = cursor.digits(2)?;
    Some(IsoDate {
        year,
        month: u8::try_from(month).ok()?,
        day: u8::try_from(day).ok()?,
    })
    .filter(|date| (1..=12).contains(&date.month) && (1..=31).contains(&date.day))
}

fn parse_year(cursor: &mut Cursor<'_>) -> Option<i32> {
    match cursor.peek()? {
        sign @ (b'+' | b'-') => {
            cursor.position += 1;
            let magnitude = i32::try_from(cursor.digits(6)?).ok()?;
            if sign == b'-' && magnitude == 0 {
                return None;
            }
            Some(if sign == b'-' { -magnitude } else { magnitude })
        }
        _ => i32::try_from(cursor.digits(4)?).ok(),
    }
}

/// `HH[:MM[:SS[.fraction]]]`, or the basic `HHMM[SS[.fraction]]`. A second of
/// 60 is a leap second, folded to 59.
fn parse_time(cursor: &mut Cursor<'_>) -> Option<IsoTime> {
    let hour = cursor.digits(2)?;
    let extended = cursor.peek() == Some(b':');
    let mut minute = 0;
    let mut second = 0;
    let mut nanos = 0;
    if extended || cursor.peek().is_some_and(|byte| byte.is_ascii_digit()) {
        if extended {
            cursor.position += 1;
        }
        minute = cursor.digits(2)?;
        let has_second = if extended {
            cursor.eat_one_of(b":")
        } else {
            cursor.peek().is_some_and(|byte| byte.is_ascii_digit())
        };
        if has_second {
            second = cursor.digits(2)?;
            if cursor.eat_one_of(b".,") {
                let digits = cursor.fraction_digits()?;
                nanos = Cursor::fraction_nanos(&digits);
            }
        }
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let second = second.min(59);
    Some(IsoTime {
        hour: hour as u8,
        minute: minute as u8,
        second: second as u8,
        millisecond: (nanos / 1_000_000) as u16,
        microsecond: (nanos / 1_000 % 1_000) as u16,
        nanosecond: (nanos % 1_000) as u16,
    })
}

/// `Z`, or `±HH[[:]MM[[:]SS[.fraction]]]`, as nanoseconds. Returns the offset
/// and whether it was the `Z` designator.
fn parse_offset(cursor: &mut Cursor<'_>) -> Option<(i128, bool)> {
    if cursor.eat_one_of(b"Zz") {
        return Some((0, true));
    }
    let sign: i128 = match cursor.peek()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    cursor.position += 1;
    let hours = cursor.digits(2)?;
    let mut minutes = 0;
    let mut seconds = 0;
    let mut nanos = 0;
    if hours > 23 {
        return None;
    }
    let extended = cursor.peek() == Some(b':');
    if extended || cursor.peek().is_some_and(|byte| byte.is_ascii_digit()) {
        if extended {
            cursor.position += 1;
        }
        minutes = cursor.digits(2)?;
        let has_seconds = if extended {
            cursor.eat_one_of(b":")
        } else {
            cursor.peek().is_some_and(|byte| byte.is_ascii_digit())
        };
        if has_seconds {
            seconds = cursor.digits(2)?;
            if cursor.eat_one_of(b".,") {
                let digits = cursor.fraction_digits()?;
                nanos = Cursor::fraction_nanos(&digits);
            }
        }
    }
    if minutes > 59 || seconds > 59 {
        return None;
    }
    let total = i128::from(hours) * 3_600_000_000_000
        + i128::from(minutes) * 60_000_000_000
        + i128::from(seconds) * 1_000_000_000
        + i128::from(nanos);
    Some((sign * total, false))
}

/// The bracketed annotations that follow a date or time. Returns the calendar
/// annotation and whether a time zone annotation was seen. A critical
/// annotation the grammar does not know is a refusal.
fn parse_annotations(cursor: &mut Cursor<'_>) -> Option<(Option<String>, bool)> {
    // Each calendar annotation with its criticality, in the order written.
    let mut calendars: Vec<(String, bool)> = Vec::new();
    let mut time_zone = false;
    while cursor.eat_one_of(b"[") {
        let critical = cursor.eat_one_of(b"!");
        let start = cursor.position;
        while cursor
            .peek()
            .is_some_and(|byte| byte != b'=' && byte != b']')
        {
            cursor.position += 1;
        }
        let body = std::str::from_utf8(&cursor.bytes[start..cursor.position]).ok()?;
        if cursor.eat_one_of(b"=") {
            let value_start = cursor.position;
            while cursor.peek().is_some_and(|byte| byte != b']') {
                cursor.position += 1;
            }
            let value = std::str::from_utf8(&cursor.bytes[value_start..cursor.position]).ok()?;
            if !is_annotation_key(body) || !is_annotation_value(value) {
                return None;
            }
            if body == "u-ca" {
                calendars.push((value.to_owned(), critical));
            } else if critical {
                return None;
            }
        } else {
            // A bare annotation names the time zone: at most one, and a numeric
            // offset in it may carry no seconds.
            if time_zone || !is_time_zone_annotation(body) {
                return None;
            }
            time_zone = true;
        }
        if !cursor.eat_one_of(b"]") {
            return None;
        }
    }
    // Repeated calendars must agree, and may not repeat at all if any is critical.
    if let Some((first, _)) = calendars.first() {
        let any_critical = calendars.iter().any(|(_, critical)| *critical);
        if any_critical && calendars.len() > 1 {
            return None;
        }
        if calendars.iter().any(|(value, _)| value != first) {
            return None;
        }
    }
    Some((
        calendars.into_iter().next().map(|(value, _)| value),
        time_zone,
    ))
}

/// Whether the annotations in `rest` include a key other than `u-ca`. A
/// year-month or month-day string takes only the calendar and time zone
/// annotations, so any other key makes it invalid.
fn has_unknown_annotation(rest: &[u8]) -> bool {
    rest.split(|byte| *byte == b'[').skip(1).any(|group| {
        let body = group.split(|byte| *byte == b']').next().unwrap_or_default();
        let body = body.strip_prefix(b"!").unwrap_or(body);
        body.iter()
            .position(|byte| *byte == b'=')
            .is_some_and(|split| &body[..split] != b"u-ca")
    })
}

/// An annotation key: a lowercase letter or `_`, then lowercase letters,
/// digits, `-` or `_`.
fn is_annotation_key(key: &str) -> bool {
    let mut chars = key.bytes();
    matches!(chars.next(), Some(b'a'..=b'z' | b'_'))
        && chars.all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
}

/// An annotation value: one or more alphanumeric runs joined by `-`.
fn is_annotation_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

/// A bracketed time zone: a numeric offset of hours and minutes, or a zone
/// name that starts with a letter.
fn is_time_zone_annotation(body: &str) -> bool {
    match body.as_bytes().first() {
        Some(b'+' | b'-') => {
            let digits: String = body[1..]
                .chars()
                .filter(|character| *character != ':')
                .collect();
            digits.len() == 4
                && digits.bytes().all(|byte| byte.is_ascii_digit())
                && digits[..2].parse::<u32>().is_ok_and(|hours| hours <= 23)
                && digits[2..]
                    .parse::<u32>()
                    .is_ok_and(|minutes| minutes <= 59)
                && (body.len() == 5 || (body.len() == 6 && body.as_bytes()[3] == b':'))
        }
        Some(byte) if byte.is_ascii_alphabetic() => body.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'+' | b'.')
        }),
        _ => false,
    }
}

/// A complete date-time string, with the time, offset and annotations that
/// are present. The separator is `T`, `t` or a space.
pub(crate) fn parse_date_time(text: &str) -> Option<ParsedDateTime> {
    let mut cursor = Cursor::new(text);
    let date = parse_date(&mut cursor)?;
    let mut parsed = ParsedDateTime {
        date,
        time: None,
        offset: None,
        utc: false,
        calendar: None,
        time_zone: false,
    };
    if cursor.eat_one_of(b"Tt ") {
        parsed.time = Some(parse_time(&mut cursor)?);
        if !matches!(cursor.peek(), None | Some(b'[')) {
            let (offset, utc) = parse_offset(&mut cursor)?;
            parsed.offset = Some(offset);
            parsed.utc = utc;
        }
    }
    let (calendar, time_zone) = parse_annotations(&mut cursor)?;
    parsed.calendar = calendar;
    parsed.time_zone = time_zone;
    cursor.at_end().then_some(parsed)
}

/// A time string: `T` optional, then `HH:MM[:SS]`, with an offset and
/// annotations allowed. A full date-time string is also accepted, and its
/// time is the one returned.
pub(crate) fn parse_time_string(text: &str) -> Option<ParsedDateTime> {
    let mut cursor = Cursor::new(text);
    if cursor.eat_one_of(b"Tt") {
        let time = parse_time(&mut cursor)?;
        return finish_time(cursor, time);
    }
    if let Some(parsed) = parse_date_time(text) {
        return parsed.time.is_some().then_some(parsed);
    }
    // Without a `T`, an hour and minute in basic form (`HHMM`) reads as the
    // start of a year-month or date, so it is ambiguous and refused.
    let head = text.split('[').next().unwrap_or_default();
    let basic_digits = head.bytes().take_while(u8::is_ascii_digit).count();
    if !head.contains(':') && basic_digits < 6 {
        return None;
    }
    let time = parse_time(&mut cursor)?;
    finish_time(cursor, time)
}

fn finish_time(mut cursor: Cursor<'_>, time: IsoTime) -> Option<ParsedDateTime> {
    let mut parsed = ParsedDateTime {
        date: IsoDate {
            year: 1972,
            month: 1,
            day: 1,
        },
        time: Some(time),
        offset: None,
        utc: false,
        calendar: None,
        time_zone: false,
    };
    if !matches!(cursor.peek(), None | Some(b'[')) {
        let (offset, utc) = parse_offset(&mut cursor)?;
        parsed.offset = Some(offset);
        parsed.utc = utc;
    }
    let (calendar, time_zone) = parse_annotations(&mut cursor)?;
    parsed.calendar = calendar;
    parsed.time_zone = time_zone;
    cursor.at_end().then_some(parsed)
}

/// A year and month, `YYYY-MM` or `YYYYMM`, or a full date string whose day
/// is discarded. The returned date's day is the reference day `1`.
pub(crate) fn parse_year_month(text: &str) -> Option<ParsedDateTime> {
    let mut cursor = Cursor::new(text);
    let year = parse_year(&mut cursor)?;
    let extended = cursor.eat_one_of(b"-");
    let month = cursor.digits(2)?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let _ = extended;
    if cursor.at_end() || cursor.peek() == Some(b'[') {
        if has_unknown_annotation(&cursor.bytes[cursor.position..]) {
            return None;
        }
        let (calendar, time_zone) = parse_annotations(&mut cursor)?;
        return cursor.at_end().then_some(ParsedDateTime {
            date: IsoDate {
                year,
                month: month as u8,
                day: 1,
            },
            time: None,
            offset: None,
            utc: false,
            calendar,
            time_zone,
        });
    }
    parse_date_time(text)
}

/// A month and day: `--MM-DD`, `--MMDD`, or a full date string whose year is
/// discarded. The returned date's year is the reference year `1972` unless the
/// caller supplies another.
pub(crate) fn parse_month_day(text: &str) -> Option<ParsedDateTime> {
    let mut cursor = Cursor::new(text);
    if cursor.peek() == Some(b'-') {
        cursor.position += 1;
        if !cursor.eat_one_of(b"-") {
            return None;
        }
        let month = cursor.digits(2)?;
        let extended = cursor.eat_one_of(b"-");
        let _ = extended;
        let day = cursor.digits(2)?;
        if has_unknown_annotation(&cursor.bytes[cursor.position..]) {
            return None;
        }
        let (calendar, time_zone) = parse_annotations(&mut cursor)?;
        if !cursor.at_end() {
            return None;
        }
        return Some(ParsedDateTime {
            date: IsoDate {
                year: 1972,
                month: u8::try_from(month).ok()?,
                day: u8::try_from(day).ok()?,
            },
            time: None,
            offset: None,
            utc: false,
            calendar,
            time_zone,
        })
        .filter(|parsed| valid_month_day(parsed.date));
    }
    if let Some(parsed) = parse_date_time(text) {
        return Some(parsed);
    }
    let month = cursor.digits(2)?;
    let day = cursor.digits(2)?;
    if has_unknown_annotation(&cursor.bytes[cursor.position..]) {
        return None;
    }
    let (calendar, time_zone) = parse_annotations(&mut cursor)?;
    cursor
        .at_end()
        .then_some(ParsedDateTime {
            date: IsoDate {
                year: 1972,
                month: u8::try_from(month).ok()?,
                day: u8::try_from(day).ok()?,
            },
            time: None,
            offset: None,
            utc: false,
            calendar,
            time_zone,
        })
        .filter(|parsed| valid_month_day(parsed.date))
}

/// A month and day can be checked for grammar only: the month must be 1 to
/// 12 and the day 1 to 31, and the calendar check is for the caller.
fn valid_month_day(date: IsoDate) -> bool {
    (1..=12).contains(&date.month) && (1..=31).contains(&date.day)
}

/// An instant string: a date-time with a mandatory `Z` or numeric offset.
pub(crate) fn parse_instant(text: &str) -> Option<ParsedDateTime> {
    let parsed = parse_date_time(text)?;
    (parsed.time.is_some() && parsed.offset.is_some()).then_some(parsed)
}

/// A duration string `[+-]P[nY][nM][nW][nD][T[nH][nM][nS]]`. Only the last
/// component may carry a fraction, and only a time component may do so.
pub(crate) fn parse_duration(text: &str) -> Option<ParsedDuration> {
    let mut cursor = Cursor::new(text);
    let mut parsed = ParsedDuration::default();
    if cursor.eat_one_of(b"+-") {
        parsed.negative = cursor.bytes[0] == b'-';
    }
    if !cursor.eat_one_of(b"Pp") {
        return None;
    }
    let mut any = false;
    let mut in_time = false;
    let mut time_seen = false;
    while !cursor.at_end() {
        if cursor.eat_one_of(b"Tt") {
            if in_time {
                return None;
            }
            in_time = true;
            continue;
        }
        let whole = cursor.digit_run()?;
        let fraction = if cursor.eat_one_of(b".,") {
            Some(cursor.fraction_digits()?)
        } else {
            None
        };
        let designator = cursor.peek()?;
        cursor.position += 1;
        let value = Component {
            whole: whole.parse::<i128>().ok()?,
            fraction: fraction.unwrap_or_default(),
        };
        let has_fraction = !value.fraction.is_empty();
        match (in_time, designator.to_ascii_uppercase()) {
            (false, b'Y') => parsed.years = Some(value),
            (false, b'M') => parsed.months = Some(value),
            (false, b'W') => parsed.weeks = Some(value),
            (false, b'D') => parsed.days = Some(value),
            (true, b'H') => {
                time_seen = true;
                parsed.hours = Some(value);
            }
            (true, b'M') => {
                time_seen = true;
                parsed.minutes = Some(value);
            }
            (true, b'S') => {
                time_seen = true;
                parsed.seconds = Some(value);
            }
            _ => return None,
        }
        if has_fraction && (!in_time || cursor.peek().is_some_and(|byte| byte.is_ascii_digit())) {
            return None;
        }
        any = true;
    }
    if in_time && !time_seen {
        return None;
    }
    any.then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_time_grammar_accepts_extended_basic_and_annotated_forms() {
        let parsed = parse_date_time("2020-01-02T03:04:05.5+01:00[u-ca=iso8601][Europe/Paris]")
            .expect("valid");
        assert_eq!(
            parsed.date,
            IsoDate {
                year: 2020,
                month: 1,
                day: 2
            }
        );
        assert_eq!(parsed.time.map(|t| t.millisecond), Some(500));
        assert_eq!(parsed.offset, Some(3_600_000_000_000));
        assert_eq!(parsed.calendar.as_deref(), Some("iso8601"));
        assert!(parsed.time_zone);
        assert!(parse_date_time("20200102T030405").is_some());
        assert!(parse_date_time("+002020-01-02").is_some());
    }

    #[test]
    fn grammar_refuses_malformed_years_leap_seconds_and_unknown_critical_annotations() {
        assert!(parse_date_time("-000000-01-01").is_none());
        assert!(parse_date_time("2020-1-02").is_none());
        assert!(parse_date_time("2020-01-02T24:00").is_none());
        assert!(parse_date_time("2020-01-02[!foo=bar]").is_none());
        assert!(parse_date_time("2020-01-02[u-ca=iso8601][u-ca=gregory]").is_none());
        let leap = parse_date_time("2020-01-02T23:59:60").expect("leap second");
        assert_eq!(leap.time.map(|t| t.second), Some(59));
    }

    #[test]
    fn instant_requires_an_offset_and_duration_takes_one_fraction() {
        assert!(parse_instant("2020-01-02T03:04Z").is_some());
        assert!(parse_instant("2020-01-02T03:04").is_none());
        let duration = parse_duration("-P1Y2M3W4DT5H6M7.5S").expect("duration");
        assert!(duration.negative);
        assert_eq!(duration.years.map(|c| c.whole), Some(1));
        assert_eq!(duration.seconds.map(|c| c.fraction), Some("5".to_owned()));
        assert!(parse_duration("P1.5Y").is_none());
        assert!(parse_duration("PT").is_none());
        assert!(parse_duration("P").is_none());
    }

    #[test]
    fn month_day_and_year_month_grammar_accepts_their_short_forms() {
        let month_day = parse_month_day("--12-31").expect("month day");
        assert_eq!((month_day.date.month, month_day.date.day), (12, 31));
        assert!(parse_month_day("--12-31[u-ca=iso8601]").is_some());
        let year_month = parse_year_month("2020-02").expect("year month");
        assert_eq!((year_month.date.year, year_month.date.month), (2020, 2));
        assert!(parse_year_month("202002").is_some());
    }
}
