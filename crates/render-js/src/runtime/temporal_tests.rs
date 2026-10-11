//! The `Temporal` namespace and its ISO-calendar, UTC-only classes, exercised
//! through script. Each assertion is a JavaScript expression whose string value
//! is compared with the expected text.

use crate::JsRuntime;
use render_html::parse_document;

/// Evaluate `source` in a fresh page and return the completion value as text.
fn ok(source: &str) -> String {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(&mut dom, source)
        .map(|outcome| outcome.value.to_js_string())
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

/// Evaluate `source`, which must throw an error named `name`, and return that name.
fn throws(source: &str) -> String {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    let wrapped =
        format!("var r; try {{ {source}; r = 'no error'; }} catch (e) {{ r = e.name; }} r");
    runtime
        .execute(&mut dom, &wrapped)
        .map(|outcome| outcome.value.to_js_string())
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

#[test]
fn namespace_and_duration_constructor_have_the_specified_shape() {
    assert_eq!(
        ok(
            "[typeof Temporal, String(Temporal), typeof Temporal.Duration, Temporal.Duration.length, Temporal.Duration.from.length, Temporal.Duration.compare.length, Temporal.Duration.name].join()"
        ),
        "object,[object Temporal],function,0,1,2,Duration"
    );
    assert_eq!(
        ok(
            "var d = Object.getOwnPropertyDescriptor(Temporal, 'Duration'); [d.writable, d.enumerable, d.configurable].join()"
        ),
        "true,false,true"
    );
    assert_eq!(
        ok("Object.prototype.toString.call(Temporal.Duration.prototype)"),
        "[object Temporal.Duration]"
    );
}

#[test]
fn constructor_stores_fields_and_formats_them_as_iso_text() {
    assert_eq!(
        ok("new Temporal.Duration(1, 2, 3, 4, 5, 6, 7, 8, 9, 10).toString()"),
        "P1Y2M3W4DT5H6M7.00800901S"
    );
    assert_eq!(ok("new Temporal.Duration().toString()"), "PT0S");
    assert_eq!(
        ok("new Temporal.Duration(-1, 0, 0, 0, 0, 0, 0, 0, 0, 0).sign"),
        "-1"
    );
    assert_eq!(throws("new Temporal.Duration(1.5)"), "RangeError");
    assert_eq!(throws("new Temporal.Duration(1, -1)"), "RangeError");
    assert_eq!(throws("Temporal.Duration(1)"), "TypeError");
}

#[test]
fn from_reads_strings_and_property_bags_in_specified_order() {
    assert_eq!(
        ok("Temporal.Duration.from('P1Y2M3W4DT5H6M7.5S').toString()"),
        "P1Y2M3W4DT5H6M7.5S"
    );
    assert_eq!(ok("Temporal.Duration.from('PT1.5H').toString()"), "PT1H30M");
    assert_eq!(
        ok(
            "var log = []; var bag = { get days() { log.push('days'); return 1; }, get years() { log.push('years'); return 2; }, get hours() { log.push('hours'); return undefined; } }; Temporal.Duration.from(bag); log.join()"
        ),
        "days,hours,years"
    );
    assert_eq!(throws("Temporal.Duration.from({})"), "TypeError");
    assert_eq!(throws("Temporal.Duration.from(5)"), "TypeError");
    assert_eq!(throws("Temporal.Duration.from('P')"), "RangeError");
}

#[test]
fn arithmetic_balances_time_and_refuses_calendar_units_without_relative_to() {
    assert_eq!(
        ok("Temporal.Duration.from({ hours: 1, minutes: 45 }).add({ minutes: 30 }).toString()"),
        "PT2H15M"
    );
    assert_eq!(
        ok("Temporal.Duration.from({ days: 2 }).add({ hours: 5 }).toString()"),
        "P2DT5H"
    );
    assert_eq!(
        ok("Temporal.Duration.from({ hours: -60 }).add({ days: -1 }).toString()"),
        "-P3DT12H"
    );
    assert_eq!(
        throws("Temporal.Duration.from({ years: 1 }).add({ days: 1 })"),
        "RangeError"
    );
    assert_eq!(
        throws("Temporal.Duration.from({ years: 1 }).add({})"),
        "TypeError"
    );
    assert_eq!(
        ok("Temporal.Duration.compare({ hours: 1 }, { minutes: 61 })"),
        "-1"
    );
}

#[test]
fn round_and_total_respect_the_unit_and_the_rounding_mode() {
    assert_eq!(
        ok(
            "Temporal.Duration.from({ seconds: 90, milliseconds: 500 }).round({ smallestUnit: 'minute' }).toString()"
        ),
        "PT2M"
    );
    assert_eq!(
        ok(
            "Temporal.Duration.from({ seconds: 90 }).round({ smallestUnit: 'minute', roundingMode: 'floor' }).toString()"
        ),
        "PT1M"
    );
    assert_eq!(
        ok("Temporal.Duration.from({ hours: 36 }).total({ unit: 'days' })"),
        "1.5"
    );
    assert_eq!(
        ok("Temporal.Duration.from({ minutes: 3 }).total('seconds')"),
        "180"
    );
    assert_eq!(
        throws("Temporal.Duration.from({ seconds: 1 }).round({})"),
        "RangeError"
    );
}

#[test]
fn instant_converts_epoch_units_and_formats_utc_text() {
    assert_eq!(
        ok(
            "[Temporal.Instant.fromEpochMilliseconds(1).epochNanoseconds, new Temporal.Instant(-1n).toString(), Temporal.Instant.from('1970-01-01T01:00:00+01:00').epochNanoseconds].join()"
        ),
        "1000000,1969-12-31T23:59:59.999999999Z,0"
    );
    assert_eq!(
        ok("Temporal.Instant.from('1970-01-01T00:00:01.5Z').toString({ smallestUnit: 'second' })"),
        "1970-01-01T00:00:01Z"
    );
    assert_eq!(throws("new Temporal.Instant(1)"), "TypeError");
    assert_eq!(
        throws("Temporal.Instant.from('2020-02-30T00:00Z')"),
        "RangeError"
    );
    assert_eq!(
        ok(
            "Temporal.Instant.fromEpochNanoseconds(0n).add({ hours: 2 }).since(Temporal.Instant.fromEpochNanoseconds(0n)).toString()"
        ),
        "PT7200S"
    );
    assert_eq!(
        throws("Temporal.Instant.from('1970-01-01T00:00Z').toString({ smallestUnit: 'hour' })"),
        "RangeError"
    );
}

#[test]
fn plain_date_constrains_or_rejects_and_differences_by_days_by_default() {
    assert_eq!(
        ok("new Temporal.PlainDate(2020, 2, 29).toString()"),
        "2020-02-29"
    );
    assert_eq!(
        ok("Temporal.PlainDate.from({ year: 2020, month: 2, day: 31 }).toString()"),
        "2020-02-29"
    );
    assert_eq!(
        throws(
            "Temporal.PlainDate.from({ year: 2020, month: 2, day: 31 }, { overflow: 'reject' })"
        ),
        "RangeError"
    );
    assert_eq!(
        throws("Temporal.PlainDate.from('2020-02-30')"),
        "RangeError"
    );
    assert_eq!(
        ok("new Temporal.PlainDate(2020, 1, 31).add({ months: 1 }).toString()"),
        "2020-02-29"
    );
    assert_eq!(
        ok(
            "new Temporal.PlainDate(2020, 1, 1).until(new Temporal.PlainDate(2021, 1, 1)).toString()"
        ),
        "P366D"
    );
    assert_eq!(
        ok(
            "new Temporal.PlainDate(2020, 1, 1).until(new Temporal.PlainDate(2021, 1, 1), { largestUnit: 'years' }).toString()"
        ),
        "P1Y"
    );
    assert_eq!(
        ok("Temporal.PlainDate.compare('2020-01-01', '2020-01-02')"),
        "-1"
    );
}

#[test]
fn plain_time_wraps_at_midnight_and_reads_fractional_seconds() {
    assert_eq!(ok("new Temporal.PlainTime(12, 34).toString()"), "12:34:00");
    assert_eq!(
        ok("Temporal.PlainTime.from('12:34:56.5').millisecond"),
        "500"
    );
    assert_eq!(
        ok("new Temporal.PlainTime(23, 0).add({ hours: 2 }).toString()"),
        "01:00:00"
    );
    assert_eq!(
        ok("new Temporal.PlainTime(23, 59, 59, 999).round({ smallestUnit: 'second' }).toString()"),
        "00:00:00"
    );
    assert_eq!(throws("new Temporal.PlainTime(25)"), "RangeError");
    assert_eq!(throws("Temporal.PlainTime.from('2021-12')"), "RangeError");
}

#[test]
fn plain_date_time_adds_carries_days_and_rounds_to_a_day() {
    assert_eq!(
        ok("new Temporal.PlainDateTime(2020, 1, 1, 12, 0).toString()"),
        "2020-01-01T12:00:00"
    );
    assert_eq!(
        ok("new Temporal.PlainDateTime(2020, 1, 1, 12, 0).add({ days: 1, hours: 13 }).toString()"),
        "2020-01-03T01:00:00"
    );
    assert_eq!(
        ok("new Temporal.PlainDateTime(2020, 1, 1, 12).round({ smallestUnit: 'day' }).toString()"),
        "2020-01-02T00:00:00"
    );
    assert_eq!(
        ok(
            "new Temporal.PlainDateTime(2020, 1, 1).until(new Temporal.PlainDateTime(2020, 1, 2, 12)).toString()"
        ),
        "P1DT12H"
    );
    assert_eq!(
        throws("new Temporal.PlainDateTime(2020, 1, 1).round({ smallestUnit: 'year' })"),
        "RangeError"
    );
}

#[test]
fn plain_year_month_and_month_day_keep_their_reference_days() {
    assert_eq!(
        ok("new Temporal.PlainYearMonth(2020, 5).toString()"),
        "2020-05"
    );
    assert_eq!(
        ok("new Temporal.PlainYearMonth(2020, 5).add({ months: 8 }).toString()"),
        "2021-01"
    );
    assert_eq!(
        ok("new Temporal.PlainYearMonth(2020, 5).toPlainDate({ day: 15 }).toString()"),
        "2020-05-15"
    );
    assert_eq!(
        ok("Temporal.PlainYearMonth.from('2020-05-15').toString()"),
        "2020-05"
    );
    assert_eq!(
        ok("new Temporal.PlainMonthDay(2, 29).toString()"),
        "--02-29"
    );
    assert_eq!(
        ok("new Temporal.PlainMonthDay(2, 29).toString({ calendarName: 'always' })"),
        "1972-02-29[u-ca=iso8601]"
    );
    assert_eq!(
        ok("new Temporal.PlainMonthDay(2, 29).toPlainDate({ year: 2021 }).toString()"),
        "2021-02-28"
    );
    assert_eq!(
        throws("Temporal.PlainMonthDay.from('--02-30')"),
        "RangeError"
    );
}

#[test]
fn now_reports_the_current_instant_and_the_utc_zone() {
    assert_eq!(
        ok(
            "[typeof Temporal.Now.instant().epochNanoseconds, Temporal.Now.timeZoneId(), Temporal.Now.plainDateISO().calendarId].join()"
        ),
        "bigint,UTC,iso8601"
    );
    assert_eq!(
        throws("Temporal.Now.plainDateISO('Europe/Paris')"),
        "RangeError"
    );
}

#[test]
fn value_of_throws_and_getters_are_accessors_on_the_prototype() {
    assert_eq!(
        throws("Temporal.Duration.from({ days: 1 }).valueOf()"),
        "TypeError"
    );
    assert_eq!(
        ok(
            "var d = Object.getOwnPropertyDescriptor(Temporal.Duration.prototype, 'hours'); [typeof d.get, d.set === undefined, d.get.name].join()"
        ),
        "function,true,get hours"
    );
    assert_eq!(ok("Temporal.Duration.from({ hours: 2 }).blank"), "false");
    assert_eq!(ok("new Temporal.Duration().blank"), "true");
}
