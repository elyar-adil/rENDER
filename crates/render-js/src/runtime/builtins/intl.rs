//! `Intl` (ECMA-402): `Collator`, `DateTimeFormat`, `NumberFormat`,
//! `PluralRules` and `Segmenter`, plus `Intl.getCanonicalLocales`.
//!
//! **Locale model.** This engine ships English data only. Every constructor
//! canonicalizes the requested locale list, but formatting and segmentation
//! use `en-US` patterns and `en` CLDR rules; `resolvedOptions().locale` reports
//! `"en-US"` rather than pretending the requested locale was honoured.
//! `supportedLocalesOf` returns the requested locales whose language is `en`.
//!
//! **Numbers.** Decimal, percent and currency styles; grouping is the en-US
//! three-digit grouping; `formatToParts` reports integer/group/decimal/fraction/
//! minusSign/plusSign/percentSign/currency/nan/infinity. Currency fraction
//! digits follow ISO 4217 for a small common set (JPY/KRW and the other
//! zero-digit currencies get 0, the rest 2); symbols are the en-US symbols.
//! Compact/scientific/engineering notation and Indian grouping are absent and
//! throw a `RangeError` rather than producing a wrong string.
//!
//! **Dates.** `en-US` field order and names. The engine has no time-zone
//! database (its `Date` treats local time as UTC), so `timeZone` accepts only
//! `UTC`/`Etc/UTC`/`GMT`; any other zone is a `RangeError`. `dateStyle` and
//! `timeStyle` are expanded to their en-US field sets. `era`, `dayPeriod` and
//! `fractionalSecondDigits` are not implemented and are rejected loudly.
//!
//! **Segmentation.** Extended grapheme clusters, UAX #29 word boundaries (with
//! `isWordLike` when a segment contains an alphanumeric code point) and UAX #29
//! sentence boundaries come from `unicode-segmentation`. Indices are UTF-16
//! code-unit offsets, as ECMAScript requires.
//!
//! **Collation.** `Intl.Collator` compares by Unicode scalar order. Locale
//! collation rules (accent/case folding, numeric ordering, tailoring) are not
//! implemented; options that ask for them are rejected instead of silently
//! sorted as plain code-point order.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::if_not_else,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::needless_ifs,
    clippy::ref_option,
    clippy::too_many_lines,
    clippy::wrong_self_convention
)]

use std::rc::Rc;

use unicode_segmentation::UnicodeSegmentation;

use crate::runtime::JsRuntime;
use crate::utf16::JsString;
use crate::value::{NativeFunction, ObjectHost};
use crate::{JsError, JsValue, ObjectId, PropertyDescriptor, Realm};
use render_dom::Dom;

/// The `Intl` classes that carry a constructor, plus the two internal
/// prototype objects a `Segments` iterable needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum IntlKind {
    Collator,
    DateTimeFormat,
    NumberFormat,
    PluralRules,
    Segmenter,
    /// `%Intl.Segmenter.prototype.segment%`'s result object.
    Segments,
    /// The iterator of a `Segments` object.
    SegmentIterator,
}

impl IntlKind {
    pub(crate) const CLASSES: [Self; 5] = [
        Self::Collator,
        Self::DateTimeFormat,
        Self::NumberFormat,
        Self::PluralRules,
        Self::Segmenter,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Collator => "Collator",
            Self::DateTimeFormat => "DateTimeFormat",
            Self::NumberFormat => "NumberFormat",
            Self::PluralRules => "PluralRules",
            Self::Segmenter => "Segmenter",
            Self::Segments => "Segments",
            Self::SegmentIterator => "Segment Iterator",
        }
    }
}

/// The methods and accessors that dispatch to `dispatch_intl_native`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntlOp {
    GetCanonicalLocales,
    SupportedLocalesOf,
    ResolvedOptions,
    /// The `format`/`compare` accessors, which return a bound callable.
    NumberFormatFormat,
    NumberFormatBoundFormat,
    NumberFormatFormatToParts,
    DateTimeFormatFormat,
    DateTimeFormatBoundFormat,
    DateTimeFormatFormatToParts,
    CollatorCompare,
    CollatorBoundCompare,
    PluralRulesSelect,
    SegmenterSegment,
    SegmentsContaining,
    SegmentsToArray,
    SegmentsIterator,
    SegmentIteratorNext,
    /// `%SegmentsIteratorPrototype%[@@iterator]`: the iterator is its own
    /// iterator.
    SegmentIteratorSelf,
}

/// One `Intl` instance's internal slots.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum IntlValue {
    Collator {
        locale: String,
    },
    NumberFormat(NumberFormatOptions),
    DateTimeFormat(DateTimeFormatOptions),
    PluralRules(PluralRulesOptions),
    Segmenter(SegmenterOptions),
    Segments {
        input: JsString,
        entries: Rc<Vec<IntlSegment>>,
    },
    SegmentIterator {
        input: JsString,
        entries: Rc<Vec<IntlSegment>>,
        index: usize,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NumberFormatOptions {
    pub(crate) locale: String,
    pub(crate) style: NumberStyle,
    pub(crate) notation: NumberNotation,
    pub(crate) use_grouping: bool,
    pub(crate) minimum_integer_digits: u8,
    pub(crate) minimum_fraction_digits: u8,
    pub(crate) maximum_fraction_digits: u8,
    /// Compact notation with no explicit fraction-digit option uses CLDR's
    /// "more precision of 0 fraction digits or 2 significant digits" rule.
    pub(crate) compact_default_rounding: bool,
    pub(crate) currency_display: CurrencyDisplay,
    pub(crate) sign_display: SignDisplay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NumberNotation {
    Standard,
    Compact,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NumberStyle {
    Decimal,
    Percent,
    Currency(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CurrencyDisplay {
    Symbol,
    Code,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignDisplay {
    Auto,
    Never,
    Always,
    ExceptZero,
    Negative,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DateTimeFormatOptions {
    pub(crate) locale: String,
    pub(crate) year: Option<NumericStyle>,
    pub(crate) month: Option<MonthStyle>,
    pub(crate) day: Option<NumericStyle>,
    pub(crate) weekday: Option<WeekdayStyle>,
    pub(crate) hour: Option<NumericStyle>,
    pub(crate) minute: Option<NumericStyle>,
    pub(crate) second: Option<NumericStyle>,
    pub(crate) hour12: bool,
    pub(crate) hour_cycle: HourCycle,
    pub(crate) time_zone_name: Option<TimeZoneNameStyle>,
    pub(crate) date_style: Option<String>,
    pub(crate) time_style: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NumericStyle {
    Numeric,
    TwoDigit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MonthStyle {
    Numeric,
    TwoDigit,
    Long,
    Short,
    Narrow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WeekdayStyle {
    Long,
    Short,
    Narrow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimeZoneNameStyle {
    Short,
    Long,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HourCycle {
    H11,
    H12,
    H23,
    H24,
}

impl HourCycle {
    const fn is_12_hour(self) -> bool {
        matches!(self, Self::H11 | Self::H12)
    }

    const fn name(self) -> &'static str {
        match self {
            Self::H11 => "h11",
            Self::H12 => "h12",
            Self::H23 => "h23",
            Self::H24 => "h24",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PluralRulesOptions {
    pub(crate) locale: String,
    pub(crate) ordinal: bool,
    pub(crate) minimum_fraction_digits: u8,
    pub(crate) maximum_fraction_digits: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SegmenterOptions {
    pub(crate) locale: String,
    pub(crate) granularity: Granularity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Granularity {
    Grapheme,
    Word,
    Sentence,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct IntlSegment {
    pub(crate) segment: JsString,
    /// UTF-16 code-unit offset, the index ECMAScript observes.
    pub(crate) index: usize,
    /// `Some` only for word granularity.
    pub(crate) word_like: Option<bool>,
}

impl IntlSegment {
    fn end(&self) -> usize {
        self.index + self.segment.len()
    }
}

impl JsRuntime {
    /// Route one `NativeFunction::Intl` operation.
    pub(in crate::runtime) fn dispatch_intl_native(
        &mut self,
        dom: &mut Dom,
        operation: IntlOp,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match operation {
            IntlOp::GetCanonicalLocales => {
                let locales = self.intl_canonicalize_locale_list(dom, arguments.first())?;
                let values: Vec<JsValue> = locales
                    .into_iter()
                    .map(|locale| JsValue::String(locale.into()))
                    .collect();
                let array = self.create_array_from_values(&values)?;
                Ok(JsValue::Object(array))
            }
            IntlOp::SupportedLocalesOf => {
                let locales = self.intl_canonicalize_locale_list(dom, arguments.first())?;
                let values: Vec<JsValue> = locales
                    .into_iter()
                    .filter(|locale| locale == "en" || locale.starts_with("en-"))
                    .map(|locale| JsValue::String(locale.into()))
                    .collect();
                let array = self.create_array_from_values(&values)?;
                Ok(JsValue::Object(array))
            }
            IntlOp::ResolvedOptions => {
                let value = self.intl_receiver(receiver)?;
                self.intl_resolved_options(&value)
            }
            IntlOp::NumberFormatFormat => {
                self.intl_number_format_receiver(receiver)?;
                self.intl_bound_function(receiver, IntlOp::NumberFormatBoundFormat, "format", 1)
            }
            IntlOp::NumberFormatBoundFormat => {
                let options = self.intl_number_format_receiver(receiver)?;
                let number = self.intl_to_number(dom, arguments.first())?;
                let parts = number_format_parts(&options, number);
                Ok(JsValue::String(
                    parts
                        .into_iter()
                        .map(|(_, value)| value)
                        .collect::<String>()
                        .into(),
                ))
            }
            IntlOp::NumberFormatFormatToParts => {
                let options = self.intl_number_format_receiver(receiver)?;
                let number = self.intl_to_number(dom, arguments.first())?;
                let parts = number_format_parts(&options, number);
                self.intl_parts_array(parts)
            }
            IntlOp::DateTimeFormatFormat => {
                self.intl_date_time_format_receiver(receiver)?;
                self.intl_bound_function(receiver, IntlOp::DateTimeFormatBoundFormat, "format", 1)
            }
            IntlOp::DateTimeFormatBoundFormat => {
                let options = self.intl_date_time_format_receiver(receiver)?;
                let time = self.intl_to_number(dom, arguments.first())?;
                let parts = self.date_time_format_parts(&options, time)?;
                Ok(JsValue::String(
                    parts
                        .into_iter()
                        .map(|(_, value)| value)
                        .collect::<String>()
                        .into(),
                ))
            }
            IntlOp::DateTimeFormatFormatToParts => {
                let options = self.intl_date_time_format_receiver(receiver)?;
                let time = self.intl_to_number(dom, arguments.first())?;
                let parts = self.date_time_format_parts(&options, time)?;
                self.intl_parts_array(parts)
            }
            IntlOp::CollatorCompare => {
                self.intl_collator_receiver(receiver)?;
                self.intl_bound_function(receiver, IntlOp::CollatorBoundCompare, "compare", 2)
            }
            IntlOp::CollatorBoundCompare => {
                self.intl_collator_receiver(receiver)?;
                let left = self.intl_to_string(dom, arguments.first())?;
                let right = self.intl_to_string(dom, arguments.get(1))?;
                let ordering = left.cmp(&right);
                Ok(JsValue::Number(match ordering {
                    std::cmp::Ordering::Less => -1.0,
                    std::cmp::Ordering::Equal => 0.0,
                    std::cmp::Ordering::Greater => 1.0,
                }))
            }
            IntlOp::PluralRulesSelect => {
                let options = self.intl_plural_rules_receiver(receiver)?;
                let number = self.intl_to_number(dom, arguments.first())?;
                Ok(JsValue::String(plural_category(&options, number).into()))
            }
            IntlOp::SegmenterSegment => {
                let options = self.intl_segmenter_receiver(receiver)?;
                // `segment` takes a USVString: an unpaired surrogate becomes
                // U+FFFD before segmentation, and the reported `input` must
                // match the text the indices address.
                let input = JsString::from(self.intl_to_string(dom, arguments.first())?.to_lossy());
                let entries = Rc::new(segment_string(&input, options.granularity));
                let prototype = self.realm.intl_prototype(IntlKind::Segments);
                self.ensure_heap_capacity(1)?;
                let object = self.realm.allocate_host(
                    Some(prototype),
                    ObjectHost::Intl(IntlValue::Segments { input, entries }),
                );
                Ok(JsValue::Object(object))
            }
            IntlOp::SegmentsContaining => self.intl_segments_containing(dom, receiver, arguments),
            IntlOp::SegmentsToArray => self.intl_segments_to_array(receiver),
            IntlOp::SegmentsIterator => self.intl_segments_iterator(receiver),
            IntlOp::SegmentIteratorNext => self.intl_segment_iterator_next(receiver),
            IntlOp::SegmentIteratorSelf => match self.intl_receiver(receiver)? {
                IntlValue::SegmentIterator { .. } => Ok(JsValue::Object(receiver)),
                _ => Err(JsError::type_error("Illegal invocation")),
            },
        }
    }

    /// A fresh object with `prototype` and the parsed `Intl` slots.
    pub(in crate::runtime) fn intl_construct(
        &mut self,
        dom: &mut Dom,
        kind: IntlKind,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let locales = arguments.first();
        let options = arguments.get(1);
        let value = match kind {
            IntlKind::Collator => self.intl_collator_options(dom, locales, options)?,
            IntlKind::DateTimeFormat => IntlValue::DateTimeFormat(
                self.intl_date_time_format_options(dom, locales, options)?,
            ),
            IntlKind::NumberFormat => {
                IntlValue::NumberFormat(self.intl_number_format_options(dom, locales, options)?)
            }
            IntlKind::PluralRules => {
                IntlValue::PluralRules(self.intl_plural_rules_options(dom, locales, options)?)
            }
            IntlKind::Segmenter => {
                IntlValue::Segmenter(self.intl_segmenter_options(dom, locales, options)?)
            }
            IntlKind::Segments | IntlKind::SegmentIterator => {
                return Err(JsError::type_error("Illegal constructor"));
            }
        };
        let fallback = self.realm.intl_prototype(kind);
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
            .unwrap_or(fallback);
        self.ensure_heap_capacity(1)?;
        let object = self
            .realm
            .allocate_host(Some(prototype), ObjectHost::Intl(value));
        Ok(JsValue::Object(object))
    }

    fn intl_receiver(&self, receiver: ObjectId) -> Result<IntlValue, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::Intl(value)) => Ok(value.clone()),
            _ => Err(JsError::type_error("Illegal invocation")),
        }
    }

    fn intl_number_format_receiver(
        &self,
        receiver: ObjectId,
    ) -> Result<NumberFormatOptions, JsError> {
        match self.intl_receiver(receiver)? {
            IntlValue::NumberFormat(options) => Ok(options),
            _ => Err(JsError::type_error("Illegal invocation")),
        }
    }

    fn intl_date_time_format_receiver(
        &self,
        receiver: ObjectId,
    ) -> Result<DateTimeFormatOptions, JsError> {
        match self.intl_receiver(receiver)? {
            IntlValue::DateTimeFormat(options) => Ok(options),
            _ => Err(JsError::type_error("Illegal invocation")),
        }
    }

    fn intl_collator_receiver(&self, receiver: ObjectId) -> Result<(), JsError> {
        match self.intl_receiver(receiver)? {
            IntlValue::Collator { .. } => Ok(()),
            _ => Err(JsError::type_error("Illegal invocation")),
        }
    }

    fn intl_plural_rules_receiver(
        &self,
        receiver: ObjectId,
    ) -> Result<PluralRulesOptions, JsError> {
        match self.intl_receiver(receiver)? {
            IntlValue::PluralRules(options) => Ok(options),
            _ => Err(JsError::type_error("Illegal invocation")),
        }
    }

    fn intl_segmenter_receiver(&self, receiver: ObjectId) -> Result<SegmenterOptions, JsError> {
        match self.intl_receiver(receiver)? {
            IntlValue::Segmenter(options) => Ok(options),
            _ => Err(JsError::type_error("Illegal invocation")),
        }
    }

    fn intl_to_number(&mut self, dom: &mut Dom, value: Option<&JsValue>) -> Result<f64, JsError> {
        let value = value.cloned().unwrap_or(JsValue::Undefined);
        self.to_number_value(dom, &value)
    }

    fn intl_to_string(
        &mut self,
        dom: &mut Dom,
        value: Option<&JsValue>,
    ) -> Result<JsString, JsError> {
        let value = value.cloned().unwrap_or(JsValue::Undefined);
        self.to_string_value(dom, &value)
    }

    fn intl_locale(&mut self, dom: &mut Dom, locales: Option<&JsValue>) -> Result<String, JsError> {
        // Validate and canonicalize the request, then report the locale whose
        // data is actually used. There is no non-English formatter to resolve
        // a request to, and claiming the requested locale was resolved would
        // be a lie about the patterns in the output.
        let _ = self.intl_canonicalize_locale_list(dom, locales)?;
        Ok("en-US".to_owned())
    }

    fn intl_canonicalize_locale_list(
        &mut self,
        dom: &mut Dom,
        locales: Option<&JsValue>,
    ) -> Result<Vec<String>, JsError> {
        let Some(locales) = locales else {
            return Ok(Vec::new());
        };
        let raw = match locales {
            JsValue::Undefined => return Ok(Vec::new()),
            JsValue::String(text) => vec![text.to_lossy()],
            JsValue::Object(object) => {
                let length_value = self.get_member(dom, *object, "length")?;
                let length = self.to_number_value(dom, &length_value)?;
                if length.is_nan() || length <= 0.0 {
                    return Ok(Vec::new());
                }
                if !length.is_finite() || length > 4_096.0 {
                    return Err(self.range_error("locales is too long"));
                }
                let length = length.trunc() as usize;
                let mut values = Vec::with_capacity(length);
                for index in 0..length {
                    let value = self.get_member(dom, *object, &index.to_string())?;
                    let text = self.to_string_value(dom, &value)?;
                    values.push(text.to_lossy());
                }
                values
            }
            _ => {
                return Err(JsError::type_error(
                    "Intl locales must be a string or an array of strings",
                ));
            }
        };
        let mut result = Vec::with_capacity(raw.len());
        for locale in raw {
            let Some(canonical) = canonicalize_locale(&locale) else {
                return Err(self.range_error(&format!("Invalid language tag: {locale}")));
            };
            if !result.contains(&canonical) {
                result.push(canonical);
            }
        }
        Ok(result)
    }

    fn intl_options_object(value: Option<&JsValue>) -> Result<Option<JsValue>, JsError> {
        match value {
            None | Some(JsValue::Undefined) => Ok(None),
            Some(JsValue::Object(_)) => Ok(value.cloned()),
            _ => Err(JsError::type_error("Intl options must be an object")),
        }
    }

    fn intl_get_option(
        &mut self,
        dom: &mut Dom,
        options: &Option<JsValue>,
        name: &str,
    ) -> Result<Option<JsValue>, JsError> {
        let Some(JsValue::Object(object)) = options else {
            return Ok(None);
        };
        let value = self.get_member(dom, *object, name)?;
        if matches!(value, JsValue::Undefined) {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    fn intl_string_option(
        &mut self,
        dom: &mut Dom,
        options: &Option<JsValue>,
        name: &str,
    ) -> Result<Option<String>, JsError> {
        match self.intl_get_option(dom, options, name)? {
            Some(value) => Ok(Some(self.to_string_value(dom, &value)?.to_lossy())),
            None => Ok(None),
        }
    }

    fn intl_boolean_option(
        &mut self,
        dom: &mut Dom,
        options: &Option<JsValue>,
        name: &str,
    ) -> Result<Option<bool>, JsError> {
        let Some(value) = self.intl_get_option(dom, options, name)? else {
            return Ok(None);
        };
        Ok(Some(value.is_truthy()))
    }

    fn intl_integer_option(
        &mut self,
        dom: &mut Dom,
        options: &Option<JsValue>,
        name: &str,
        minimum: u8,
        maximum: u8,
    ) -> Result<Option<u8>, JsError> {
        let Some(value) = self.intl_get_option(dom, options, name)? else {
            return Ok(None);
        };
        let number = self.to_number_value(dom, &value)?;
        if !number.is_finite() {
            return Err(self.range_error(&format!("{name} is not finite")));
        }
        let number = number.trunc();
        if number < f64::from(minimum) || number > f64::from(maximum) {
            return Err(self.range_error(&format!("{name} is out of range")));
        }
        Ok(Some(number as u8))
    }

    fn intl_numeric_style_option(
        &mut self,
        dom: &mut Dom,
        options: &Option<JsValue>,
        name: &str,
    ) -> Result<Option<NumericStyle>, JsError> {
        match self.intl_string_option(dom, options, name)?.as_deref() {
            None => Ok(None),
            Some("numeric") => Ok(Some(NumericStyle::Numeric)),
            Some("2-digit") => Ok(Some(NumericStyle::TwoDigit)),
            Some(_) => Err(self.range_error(&format!("Invalid {name} style"))),
        }
    }

    fn intl_month_style_option(
        &mut self,
        dom: &mut Dom,
        options: &Option<JsValue>,
    ) -> Result<Option<MonthStyle>, JsError> {
        match self.intl_string_option(dom, options, "month")?.as_deref() {
            None => Ok(None),
            Some("numeric") => Ok(Some(MonthStyle::Numeric)),
            Some("2-digit") => Ok(Some(MonthStyle::TwoDigit)),
            Some("long") => Ok(Some(MonthStyle::Long)),
            Some("short") => Ok(Some(MonthStyle::Short)),
            Some("narrow") => Ok(Some(MonthStyle::Narrow)),
            Some(_) => Err(self.range_error("Invalid month style")),
        }
    }

    fn intl_weekday_style_option(
        &mut self,
        dom: &mut Dom,
        options: &Option<JsValue>,
    ) -> Result<Option<WeekdayStyle>, JsError> {
        match self.intl_string_option(dom, options, "weekday")?.as_deref() {
            None => Ok(None),
            Some("long") => Ok(Some(WeekdayStyle::Long)),
            Some("short") => Ok(Some(WeekdayStyle::Short)),
            Some("narrow") => Ok(Some(WeekdayStyle::Narrow)),
            Some(_) => Err(self.range_error("Invalid weekday style")),
        }
    }

    fn intl_time_zone_name_option(
        &mut self,
        dom: &mut Dom,
        options: &Option<JsValue>,
    ) -> Result<Option<TimeZoneNameStyle>, JsError> {
        match self
            .intl_string_option(dom, options, "timeZoneName")?
            .as_deref()
        {
            None => Ok(None),
            Some("short") => Ok(Some(TimeZoneNameStyle::Short)),
            Some("long") => Ok(Some(TimeZoneNameStyle::Long)),
            Some(_) => Err(self.range_error("Invalid timeZoneName style")),
        }
    }

    fn intl_collator_options(
        &mut self,
        dom: &mut Dom,
        locales: Option<&JsValue>,
        options: Option<&JsValue>,
    ) -> Result<IntlValue, JsError> {
        let locale = self.intl_locale(dom, locales)?;
        let options = Self::intl_options_object(options)?;
        if let Some(usage) = self.intl_string_option(dom, &options, "usage")?
            && usage != "sort"
        {
            return Err(self.range_error("Intl.Collator usage 'search' is not implemented"));
        }
        if let Some(sensitivity) = self.intl_string_option(dom, &options, "sensitivity")?
            && sensitivity != "variant"
        {
            return Err(self.range_error("Intl.Collator only implements sensitivity 'variant'"));
        }
        if self.intl_boolean_option(dom, &options, "ignorePunctuation")? == Some(true) {
            return Err(self.range_error("Intl.Collator ignorePunctuation is not implemented"));
        }
        if self.intl_boolean_option(dom, &options, "numeric")? == Some(true) {
            return Err(self.range_error("Intl.Collator numeric ordering is not implemented"));
        }
        if let Some(case_first) = self.intl_string_option(dom, &options, "caseFirst")?
            && case_first != "false"
        {
            return Err(self.range_error("Intl.Collator caseFirst is not implemented"));
        }
        if let Some(collation) = self.intl_string_option(dom, &options, "collation")?
            && collation != "default"
        {
            return Err(
                self.range_error("Intl.Collator locale collation tailoring is not implemented")
            );
        }
        Ok(IntlValue::Collator { locale })
    }

    fn intl_number_format_options(
        &mut self,
        dom: &mut Dom,
        locales: Option<&JsValue>,
        options: Option<&JsValue>,
    ) -> Result<NumberFormatOptions, JsError> {
        let locale = self.intl_locale(dom, locales)?;
        let options = Self::intl_options_object(options)?;
        let style = self
            .intl_string_option(dom, &options, "style")?
            .unwrap_or_else(|| "decimal".to_owned());
        let currency = self.intl_string_option(dom, &options, "currency")?;
        let style = match style.as_str() {
            "decimal" => {
                if currency.is_some() {
                    return Err(JsError::type_error(
                        "currency is only valid with style 'currency'",
                    ));
                }
                NumberStyle::Decimal
            }
            "percent" => {
                if currency.is_some() {
                    return Err(JsError::type_error(
                        "currency is only valid with style 'currency'",
                    ));
                }
                NumberStyle::Percent
            }
            "currency" => {
                let Some(code) = currency else {
                    return Err(self.range_error("currency is required with style 'currency'"));
                };
                if code.len() != 3 || !code.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                    return Err(self.range_error("Invalid currency code"));
                }
                NumberStyle::Currency(code.to_ascii_uppercase())
            }
            _ => return Err(self.range_error("Invalid number format style")),
        };
        let notation = match self
            .intl_string_option(dom, &options, "notation")?
            .as_deref()
        {
            None | Some("standard") => NumberNotation::Standard,
            Some("compact") => NumberNotation::Compact,
            Some(_) => {
                return Err(self.range_error(
                    "Only standard and compact notation are implemented by Intl.NumberFormat",
                ));
            }
        };
        let compact_display = self
            .intl_string_option(dom, &options, "compactDisplay")?
            .unwrap_or_else(|| "short".to_owned());
        if compact_display != "short" {
            return Err(
                self.range_error("Only short compactDisplay is implemented by Intl.NumberFormat")
            );
        }
        if self
            .intl_get_option(dom, &options, "maximumSignificantDigits")?
            .is_some()
        {
            return Err(self
                .range_error("maximumSignificantDigits is not implemented by Intl.NumberFormat"));
        }
        if self
            .intl_get_option(dom, &options, "minimumSignificantDigits")?
            .is_some()
        {
            return Err(self
                .range_error("minimumSignificantDigits is not implemented by Intl.NumberFormat"));
        }
        if let Some(mode) = self.intl_string_option(dom, &options, "roundingMode")?
            && mode != "halfExpand"
        {
            return Err(
                self.range_error("Intl.NumberFormat only implements roundingMode 'halfExpand'")
            );
        }
        if let Some(sign) = self.intl_string_option(dom, &options, "currencySign")?
            && sign != "standard"
        {
            return Err(
                self.range_error("Intl.NumberFormat currencySign 'accounting' is not implemented")
            );
        }
        let currency_display = match self
            .intl_string_option(dom, &options, "currencyDisplay")?
            .as_deref()
        {
            None | Some("symbol") => CurrencyDisplay::Symbol,
            Some("code") => CurrencyDisplay::Code,
            Some(_) => {
                return Err(
                    self.range_error("Only symbol and code currencyDisplay are implemented")
                );
            }
        };
        let sign_display = match self
            .intl_string_option(dom, &options, "signDisplay")?
            .as_deref()
        {
            None | Some("auto") => SignDisplay::Auto,
            Some("never") => SignDisplay::Never,
            Some("always") => SignDisplay::Always,
            Some("exceptZero") => SignDisplay::ExceptZero,
            Some("negative") => SignDisplay::Negative,
            Some(_) => return Err(self.range_error("Invalid signDisplay")),
        };
        let use_grouping = match self.intl_get_option(dom, &options, "useGrouping")? {
            None => true,
            Some(JsValue::String(text)) => match text.to_lossy().as_str() {
                "false" => false,
                "true" | "always" | "auto" => true,
                "min2" => true,
                _ => return Err(self.range_error("Invalid useGrouping value")),
            },
            Some(value) => value.is_truthy(),
        };
        let minimum_integer_digits = self
            .intl_integer_option(dom, &options, "minimumIntegerDigits", 1, 21)?
            .unwrap_or(1);
        let minimum_fraction_digits =
            self.intl_integer_option(dom, &options, "minimumFractionDigits", 0, 100)?;
        let maximum_fraction_digits =
            self.intl_integer_option(dom, &options, "maximumFractionDigits", 0, 100)?;
        let compact_default_rounding = notation == NumberNotation::Compact
            && minimum_fraction_digits.is_none()
            && maximum_fraction_digits.is_none();
        let default_maximum = match (&style, notation) {
            (NumberStyle::Decimal, NumberNotation::Compact) => 0,
            (NumberStyle::Decimal, NumberNotation::Standard) => 3,
            (NumberStyle::Percent, _) => 0,
            (NumberStyle::Currency(code), _) => currency_digits(code),
        };
        let default_minimum = match (&style, notation) {
            (_, NumberNotation::Compact) => 0,
            (NumberStyle::Currency(code), NumberNotation::Standard) => currency_digits(code),
            (NumberStyle::Decimal | NumberStyle::Percent, NumberNotation::Standard) => 0,
        };
        let (minimum_fraction_digits, maximum_fraction_digits) =
            match (minimum_fraction_digits, maximum_fraction_digits) {
                (None, None) => (default_minimum, default_maximum),
                (Some(minimum), None) => (minimum, default_maximum.max(minimum)),
                (None, Some(maximum)) => (maximum.min(default_minimum), maximum),
                (Some(minimum), Some(maximum)) => {
                    if minimum > maximum {
                        return Err(
                            self.range_error("minimumFractionDigits exceeds maximumFractionDigits")
                        );
                    }
                    (minimum, maximum)
                }
            };
        Ok(NumberFormatOptions {
            locale,
            style,
            notation,
            use_grouping,
            minimum_integer_digits,
            minimum_fraction_digits,
            maximum_fraction_digits,
            compact_default_rounding,
            currency_display,
            sign_display,
        })
    }

    fn intl_date_time_format_options(
        &mut self,
        dom: &mut Dom,
        locales: Option<&JsValue>,
        options: Option<&JsValue>,
    ) -> Result<DateTimeFormatOptions, JsError> {
        let locale = self.intl_locale(dom, locales)?;
        let options = Self::intl_options_object(options)?;
        let date_style = self.intl_string_option(dom, &options, "dateStyle")?;
        let time_style = self.intl_string_option(dom, &options, "timeStyle")?;
        if let Some(calendar) = self.intl_string_option(dom, &options, "calendar")?
            && calendar != "gregory"
        {
            return Err(
                self.range_error("Intl.DateTimeFormat only implements the gregory calendar")
            );
        }
        if let Some(system) = self.intl_string_option(dom, &options, "numberingSystem")?
            && system != "latn"
        {
            return Err(
                self.range_error("Intl.DateTimeFormat only implements the latn numbering system")
            );
        }
        let time_zone = self.intl_string_option(dom, &options, "timeZone")?;
        if let Some(zone) = &time_zone {
            let accepted = matches!(
                zone.as_str(),
                "UTC" | "Etc/UTC" | "Etc/GMT" | "GMT" | "utc" | "gmt"
            );
            if !accepted {
                return Err(
                    self.range_error("Only UTC and the engine's local (UTC) time are implemented")
                );
            }
        }
        let year = self.intl_numeric_style_option(dom, &options, "year")?;
        let month = self.intl_month_style_option(dom, &options)?;
        let day = self.intl_numeric_style_option(dom, &options, "day")?;
        let weekday = self.intl_weekday_style_option(dom, &options)?;
        let hour = self.intl_numeric_style_option(dom, &options, "hour")?;
        let minute = self.intl_numeric_style_option(dom, &options, "minute")?;
        let second = self.intl_numeric_style_option(dom, &options, "second")?;
        let time_zone_name = self.intl_time_zone_name_option(dom, &options)?;
        if self.intl_get_option(dom, &options, "era")?.is_some() {
            return Err(self.range_error("Intl.DateTimeFormat era is not implemented"));
        }
        if self.intl_get_option(dom, &options, "dayPeriod")?.is_some() {
            return Err(self.range_error("Intl.DateTimeFormat dayPeriod is not implemented"));
        }
        if self
            .intl_get_option(dom, &options, "fractionalSecondDigits")?
            .is_some()
        {
            return Err(
                self.range_error("Intl.DateTimeFormat fractionalSecondDigits is not implemented")
            );
        }
        let hour12_option = self.intl_boolean_option(dom, &options, "hour12")?;
        let hour_cycle_option = self.intl_string_option(dom, &options, "hourCycle")?;
        if let Some(cycle) = &hour_cycle_option
            && !matches!(cycle.as_str(), "h11" | "h12" | "h23" | "h24")
        {
            return Err(self.range_error("Invalid hourCycle"));
        }
        if hour_cycle_option.is_some() && hour12_option.is_some() {
            return Err(JsError::type_error(
                "hour12 and hourCycle cannot both be present",
            ));
        }
        let hour_cycle = match hour_cycle_option.as_deref() {
            Some("h11") => HourCycle::H11,
            Some("h12") => HourCycle::H12,
            Some("h23") => HourCycle::H23,
            Some("h24") => HourCycle::H24,
            _ if hour12_option == Some(false) => HourCycle::H23,
            _ => HourCycle::H12,
        };
        let hour12 = hour_cycle.is_12_hour();
        if date_style.is_some()
            && (year.is_some() || month.is_some() || day.is_some() || weekday.is_some())
        {
            return Err(JsError::type_error(
                "dateStyle cannot be combined with date fields",
            ));
        }
        if time_style.is_some()
            && (hour.is_some() || minute.is_some() || second.is_some() || time_zone_name.is_some())
        {
            return Err(JsError::type_error(
                "timeStyle cannot be combined with time fields",
            ));
        }
        let mut result = DateTimeFormatOptions {
            locale,
            year,
            month,
            day,
            weekday,
            hour,
            minute,
            second,
            hour12,
            hour_cycle,
            time_zone_name,
            date_style: date_style.clone(),
            time_style: time_style.clone(),
        };
        if let Some(style) = date_style {
            let (year, month, day, weekday) = match style.as_str() {
                "full" => (
                    Some(NumericStyle::Numeric),
                    Some(MonthStyle::Long),
                    Some(NumericStyle::Numeric),
                    Some(WeekdayStyle::Long),
                ),
                "long" => (
                    Some(NumericStyle::Numeric),
                    Some(MonthStyle::Long),
                    Some(NumericStyle::Numeric),
                    None,
                ),
                "medium" => (
                    Some(NumericStyle::Numeric),
                    Some(MonthStyle::Short),
                    Some(NumericStyle::Numeric),
                    None,
                ),
                "short" => (
                    Some(NumericStyle::TwoDigit),
                    Some(MonthStyle::Numeric),
                    Some(NumericStyle::Numeric),
                    None,
                ),
                _ => return Err(self.range_error("Invalid dateStyle")),
            };
            result.year = year;
            result.month = month;
            result.day = day;
            result.weekday = weekday;
        }
        if let Some(style) = time_style {
            let (hour, minute, second, zone) = match style.as_str() {
                "full" => (
                    Some(NumericStyle::Numeric),
                    Some(NumericStyle::TwoDigit),
                    Some(NumericStyle::TwoDigit),
                    Some(TimeZoneNameStyle::Long),
                ),
                "long" => (
                    Some(NumericStyle::Numeric),
                    Some(NumericStyle::TwoDigit),
                    Some(NumericStyle::TwoDigit),
                    Some(TimeZoneNameStyle::Short),
                ),
                "medium" => (
                    Some(NumericStyle::Numeric),
                    Some(NumericStyle::TwoDigit),
                    Some(NumericStyle::TwoDigit),
                    None,
                ),
                "short" => (
                    Some(NumericStyle::Numeric),
                    Some(NumericStyle::TwoDigit),
                    None,
                    None,
                ),
                _ => return Err(self.range_error("Invalid timeStyle")),
            };
            result.hour = hour;
            result.minute = minute;
            result.second = second;
            result.time_zone_name = zone;
        }
        if result.year.is_none()
            && result.month.is_none()
            && result.day.is_none()
            && result.weekday.is_none()
            && result.hour.is_none()
            && result.minute.is_none()
            && result.second.is_none()
            && result.time_zone_name.is_none()
        {
            result.year = Some(NumericStyle::Numeric);
            result.month = Some(MonthStyle::Numeric);
            result.day = Some(NumericStyle::Numeric);
        }
        Ok(result)
    }

    fn intl_plural_rules_options(
        &mut self,
        dom: &mut Dom,
        locales: Option<&JsValue>,
        options: Option<&JsValue>,
    ) -> Result<PluralRulesOptions, JsError> {
        let locale = self.intl_locale(dom, locales)?;
        let options = Self::intl_options_object(options)?;
        let kind = self
            .intl_string_option(dom, &options, "type")?
            .unwrap_or_else(|| "cardinal".to_owned());
        let ordinal = match kind.as_str() {
            "cardinal" => false,
            "ordinal" => true,
            _ => return Err(self.range_error("Invalid plural rules type")),
        };
        let notation = self
            .intl_string_option(dom, &options, "notation")?
            .unwrap_or_else(|| "standard".to_owned());
        if notation != "standard" {
            return Err(
                self.range_error("Only standard notation is implemented by Intl.PluralRules")
            );
        }
        let minimum_fraction_digits = self
            .intl_integer_option(dom, &options, "minimumFractionDigits", 0, 100)?
            .unwrap_or(0);
        let maximum_fraction_digits = self
            .intl_integer_option(dom, &options, "maximumFractionDigits", 0, 100)?
            .unwrap_or(3);
        Ok(PluralRulesOptions {
            locale,
            ordinal,
            minimum_fraction_digits,
            maximum_fraction_digits,
        })
    }

    fn intl_segmenter_options(
        &mut self,
        dom: &mut Dom,
        locales: Option<&JsValue>,
        options: Option<&JsValue>,
    ) -> Result<SegmenterOptions, JsError> {
        let locale = self.intl_locale(dom, locales)?;
        let options = Self::intl_options_object(options)?;
        let granularity = match self
            .intl_string_option(dom, &options, "granularity")?
            .as_deref()
        {
            None | Some("grapheme") => Granularity::Grapheme,
            Some("word") => Granularity::Word,
            Some("sentence") => Granularity::Sentence,
            Some(_) => return Err(self.range_error("Invalid segmentation granularity")),
        };
        Ok(SegmenterOptions {
            locale,
            granularity,
        })
    }

    fn intl_resolved_options(&mut self, value: &IntlValue) -> Result<JsValue, JsError> {
        let object = self.realm.create_ordinary_object();
        macro_rules! set {
            ($name:expr, $value:expr $(,)?) => {
                self.realm.set_property(object, $name.to_owned(), $value)
            };
        }
        match value {
            IntlValue::Collator { locale } => {
                set!("locale", JsValue::String(locale.clone().into()));
                set!("usage", JsValue::String("sort".into()));
                set!("sensitivity", JsValue::String("variant".into()));
                set!("ignorePunctuation", JsValue::Boolean(false));
                set!("collation", JsValue::String("default".into()));
                set!("numeric", JsValue::Boolean(false));
                set!("caseFirst", JsValue::String("false".into()));
            }
            IntlValue::NumberFormat(options) => {
                set!("locale", JsValue::String(options.locale.clone().into()));
                set!("numberingSystem", JsValue::String("latn".into()));
                let style = match &options.style {
                    NumberStyle::Decimal => "decimal",
                    NumberStyle::Percent => "percent",
                    NumberStyle::Currency(_) => "currency",
                };
                set!("style", JsValue::String(style.into()));
                if let NumberStyle::Currency(code) = &options.style {
                    set!("currency", JsValue::String(code.clone().into()));
                    set!(
                        "currencyDisplay",
                        JsValue::String(match options.currency_display {
                            CurrencyDisplay::Symbol => "symbol".into(),
                            CurrencyDisplay::Code => "code".into(),
                        }),
                    );
                }
                set!(
                    "minimumIntegerDigits",
                    JsValue::Number(f64::from(options.minimum_integer_digits)),
                );
                set!(
                    "minimumFractionDigits",
                    JsValue::Number(f64::from(options.minimum_fraction_digits)),
                );
                set!(
                    "maximumFractionDigits",
                    JsValue::Number(f64::from(options.maximum_fraction_digits)),
                );
                set!("useGrouping", JsValue::Boolean(options.use_grouping));
                set!(
                    "notation",
                    JsValue::String(
                        match options.notation {
                            NumberNotation::Standard => "standard",
                            NumberNotation::Compact => "compact",
                        }
                        .into(),
                    ),
                );
                if options.notation == NumberNotation::Compact {
                    set!("compactDisplay", JsValue::String("short".into()));
                }
                let sign_display = match options.sign_display {
                    SignDisplay::Auto => "auto",
                    SignDisplay::Never => "never",
                    SignDisplay::Always => "always",
                    SignDisplay::ExceptZero => "exceptZero",
                    SignDisplay::Negative => "negative",
                };
                set!("signDisplay", JsValue::String(sign_display.into()));
            }
            IntlValue::DateTimeFormat(options) => {
                set!("locale", JsValue::String(options.locale.clone().into()));
                set!("calendar", JsValue::String("gregory".into()));
                set!("numberingSystem", JsValue::String("latn".into()));
                set!("timeZone", JsValue::String("UTC".into()));
                if let Some(year) = options.year {
                    set!("year", JsValue::String(numeric_style_name(year).into()));
                }
                if let Some(month) = options.month {
                    set!("month", JsValue::String(month_style_name(month).into()));
                }
                if let Some(day) = options.day {
                    set!("day", JsValue::String(numeric_style_name(day).into()));
                }
                if let Some(weekday) = options.weekday {
                    set!(
                        "weekday",
                        JsValue::String(weekday_style_name(weekday).into())
                    );
                }
                if let Some(hour) = options.hour {
                    set!("hour", JsValue::String(numeric_style_name(hour).into()));
                }
                if let Some(minute) = options.minute {
                    set!("minute", JsValue::String(numeric_style_name(minute).into()));
                }
                if let Some(second) = options.second {
                    set!("second", JsValue::String(numeric_style_name(second).into()));
                }
                if let Some(zone) = options.time_zone_name {
                    set!(
                        "timeZoneName",
                        JsValue::String(match zone {
                            TimeZoneNameStyle::Short => "short".into(),
                            TimeZoneNameStyle::Long => "long".into(),
                        }),
                    );
                }
                if let Some(style) = &options.date_style {
                    set!("dateStyle", JsValue::String(style.clone().into()));
                }
                if let Some(style) = &options.time_style {
                    set!("timeStyle", JsValue::String(style.clone().into()));
                }
                set!("hour12", JsValue::Boolean(options.hour12));
                set!(
                    "hourCycle",
                    JsValue::String(options.hour_cycle.name().into()),
                );
            }
            IntlValue::PluralRules(options) => {
                set!("locale", JsValue::String(options.locale.clone().into()));
                set!(
                    "type",
                    JsValue::String(
                        if options.ordinal {
                            "ordinal"
                        } else {
                            "cardinal"
                        }
                        .into()
                    ),
                );
                set!("minimumIntegerDigits", JsValue::Number(1.0));
                set!(
                    "minimumFractionDigits",
                    JsValue::Number(f64::from(options.minimum_fraction_digits)),
                );
                set!(
                    "maximumFractionDigits",
                    JsValue::Number(f64::from(options.maximum_fraction_digits)),
                );
                let categories: Vec<JsValue> = if options.ordinal {
                    ["few", "one", "other", "two"]
                        .into_iter()
                        .map(|category| JsValue::String(category.into()))
                        .collect()
                } else {
                    ["one", "other"]
                        .into_iter()
                        .map(|category| JsValue::String(category.into()))
                        .collect()
                };
                let array = self.create_array_from_values(&categories)?;
                set!("pluralCategories", JsValue::Object(array));
            }
            IntlValue::Segmenter(options) => {
                set!("locale", JsValue::String(options.locale.clone().into()));
                set!(
                    "granularity",
                    JsValue::String(
                        match options.granularity {
                            Granularity::Grapheme => "grapheme",
                            Granularity::Word => "word",
                            Granularity::Sentence => "sentence",
                        }
                        .into(),
                    ),
                );
            }
            IntlValue::Segments { .. } | IntlValue::SegmentIterator { .. } => {
                return Err(JsError::type_error("Illegal invocation"));
            }
        }
        Ok(JsValue::Object(object))
    }

    /// The callable an accessor such as `format` or `compare` returns: its
    /// receiver is captured now, so extracting the method keeps working.
    fn intl_bound_function(
        &mut self,
        receiver: ObjectId,
        operation: IntlOp,
        name: &str,
        arity: usize,
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let function_prototype = self.realm.function_prototype();
        let function = self.realm.allocate_host(
            Some(function_prototype),
            ObjectHost::BoundFunction {
                function: NativeFunction::Intl(operation),
                receiver,
            },
        );
        self.realm.install_function_metadata(function, name, arity);
        Ok(JsValue::Object(function))
    }

    fn intl_parts_array(&mut self, parts: Vec<(String, String)>) -> Result<JsValue, JsError> {
        let mut values = Vec::with_capacity(parts.len());
        for (kind, value) in parts {
            let object = self.realm.create_ordinary_object();
            self.realm
                .set_property(object, "type".to_owned(), JsValue::String(kind.into()));
            self.realm
                .set_property(object, "value".to_owned(), JsValue::String(value.into()));
            values.push(JsValue::Object(object));
        }
        let array = self.create_array_from_values(&values)?;
        Ok(JsValue::Object(array))
    }

    fn intl_segment_data(&mut self, input: &JsString, entry: &IntlSegment) -> ObjectId {
        let object = self.realm.create_ordinary_object();
        self.realm.set_property(
            object,
            "segment".to_owned(),
            JsValue::String(entry.segment.clone()),
        );
        self.realm.set_property(
            object,
            "index".to_owned(),
            JsValue::Number(entry.index as f64),
        );
        self.realm
            .set_property(object, "input".to_owned(), JsValue::String(input.clone()));
        if let Some(word_like) = entry.word_like {
            self.realm
                .set_property(object, "isWordLike".to_owned(), JsValue::Boolean(word_like));
        }
        object
    }

    fn intl_segments_iterator(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let (input, entries) = match self.intl_receiver(receiver)? {
            IntlValue::Segments { input, entries } => (input, entries),
            _ => return Err(JsError::type_error("Illegal invocation")),
        };
        let prototype = self.realm.intl_prototype(IntlKind::SegmentIterator);
        self.ensure_heap_capacity(1)?;
        let object = self.realm.allocate_host(
            Some(prototype),
            ObjectHost::Intl(IntlValue::SegmentIterator {
                input,
                entries,
                index: 0,
            }),
        );
        Ok(JsValue::Object(object))
    }

    fn intl_segment_iterator_next(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let (input, entry) = match self.realm.host_mut(receiver) {
            Some(ObjectHost::Intl(IntlValue::SegmentIterator {
                input,
                entries,
                index,
            })) => {
                if *index >= entries.len() {
                    return Ok(self.intl_iterator_result(JsValue::Undefined, true));
                }
                let entry = entries[*index].clone();
                *index += 1;
                (input.clone(), entry)
            }
            _ => return Err(JsError::type_error("Illegal invocation")),
        };
        let data = self.intl_segment_data(&input, &entry);
        Ok(self.intl_iterator_result(JsValue::Object(data), false))
    }

    fn intl_iterator_result(&mut self, value: JsValue, done: bool) -> JsValue {
        let object = self.realm.create_ordinary_object();
        self.realm.set_property(object, "value".to_owned(), value);
        self.realm
            .set_property(object, "done".to_owned(), JsValue::Boolean(done));
        JsValue::Object(object)
    }

    fn intl_segments_to_array(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let (input, entries) = match self.intl_receiver(receiver)? {
            IntlValue::Segments { input, entries } => (input, entries),
            _ => return Err(JsError::type_error("Illegal invocation")),
        };
        let mut values = Vec::with_capacity(entries.len());
        for entry in entries.iter() {
            let object = self.intl_segment_data(&input, entry);
            values.push(JsValue::Object(object));
        }
        let array = self.create_array_from_values(&values)?;
        Ok(JsValue::Object(array))
    }

    fn intl_segments_containing(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (input, entries) = match self.intl_receiver(receiver)? {
            IntlValue::Segments { input, entries } => (input, entries),
            _ => return Err(JsError::type_error("Illegal invocation")),
        };
        let number = self.intl_to_number(dom, arguments.first())?;
        // ToIntegerOrInfinity: NaN is +0, so `containing()` starts at index 0.
        let index = if number.is_nan() { 0.0 } else { number.trunc() };
        if index < 0.0 || index >= input.len() as f64 {
            return Ok(JsValue::Undefined);
        }
        let index = index as usize;
        let Some(entry) = entries
            .iter()
            .find(|entry| entry.index <= index && index < entry.end())
        else {
            return Ok(JsValue::Undefined);
        };
        let object = self.intl_segment_data(&input, entry);
        Ok(JsValue::Object(object))
    }

    fn date_time_format_parts(
        &mut self,
        options: &DateTimeFormatOptions,
        time: f64,
    ) -> Result<Vec<(String, String)>, JsError> {
        if !time.is_finite() {
            return Err(self.range_error("Invalid time value"));
        }
        let (year, month, day, hour, minute, second, _, weekday) = JsRuntime::date_components(time);
        let mut parts = Vec::new();
        let mut has_date = false;
        if let Some(style) = options.weekday {
            let name = match style {
                WeekdayStyle::Long => WEEKDAYS_LONG[weekday as usize],
                WeekdayStyle::Short => WEEKDAYS_SHORT[weekday as usize],
                WeekdayStyle::Narrow => WEEKDAYS_NARROW[weekday as usize],
            };
            parts.push(("weekday".to_owned(), name.to_owned()));
            has_date = true;
        }
        if let Some(month_style) = options.month {
            has_date = true;
            let numeric_month = month + 1;
            match month_style {
                MonthStyle::Numeric | MonthStyle::TwoDigit => {
                    let numeric_day_slash = options.weekday.is_none()
                        && options.day.is_some()
                        && options.year.is_some();
                    if numeric_day_slash {
                        parts.push((
                            "month".to_owned(),
                            format_month_number(numeric_month, month_style),
                        ));
                        parts.push(("literal".to_owned(), "/".to_owned()));
                        if let Some(day_style) = options.day {
                            parts.push(("day".to_owned(), format_day_number(day, day_style)));
                        }
                        parts.push(("literal".to_owned(), "/".to_owned()));
                        if let Some(year_style) = options.year {
                            parts.push(("year".to_owned(), format_year_number(year, year_style)));
                        }
                    } else {
                        if options.weekday.is_some() {
                            parts.push(("literal".to_owned(), ", ".to_owned()));
                        }
                        parts.push((
                            "month".to_owned(),
                            format_month_number(numeric_month, month_style),
                        ));
                        if let Some(day_style) = options.day {
                            parts.push(("literal".to_owned(), "/".to_owned()));
                            parts.push(("day".to_owned(), format_day_number(day, day_style)));
                        }
                        if let Some(year_style) = options.year {
                            parts.push(("literal".to_owned(), "/".to_owned()));
                            parts.push(("year".to_owned(), format_year_number(year, year_style)));
                        }
                    }
                }
                MonthStyle::Long | MonthStyle::Short | MonthStyle::Narrow => {
                    if options.weekday.is_some() {
                        parts.push(("literal".to_owned(), ", ".to_owned()));
                    }
                    let name = match month_style {
                        MonthStyle::Long => MONTHS_LONG[month as usize],
                        MonthStyle::Short => MONTHS_SHORT[month as usize],
                        MonthStyle::Narrow => MONTHS_NARROW[month as usize],
                        _ => unreachable!(),
                    };
                    parts.push(("month".to_owned(), name.to_owned()));
                    if let Some(day_style) = options.day {
                        parts.push(("literal".to_owned(), " ".to_owned()));
                        parts.push(("day".to_owned(), format_day_number(day, day_style)));
                    }
                    if let Some(year_style) = options.year {
                        parts.push(("literal".to_owned(), ", ".to_owned()));
                        parts.push(("year".to_owned(), format_year_number(year, year_style)));
                    }
                }
            }
        } else {
            if options.weekday.is_some() && (options.day.is_some() || options.year.is_some()) {
                parts.push(("literal".to_owned(), ", ".to_owned()));
            }
            if let Some(day_style) = options.day {
                has_date = true;
                parts.push(("day".to_owned(), format_day_number(day, day_style)));
            }
            if let Some(year_style) = options.year {
                has_date = true;
                if options.day.is_some() {
                    parts.push(("literal".to_owned(), ", ".to_owned()));
                }
                parts.push(("year".to_owned(), format_year_number(year, year_style)));
            }
        }
        if has_date
            && (options.hour.is_some() || options.minute.is_some() || options.second.is_some())
        {
            parts.push(("literal".to_owned(), " at ".to_owned()));
        }
        if let Some(hour_style) = options.hour {
            let (display_hour, day_period) = match options.hour_cycle {
                HourCycle::H11 => (hour % 12, Some(if hour < 12 { "AM" } else { "PM" })),
                HourCycle::H12 => {
                    let hour_portion = hour % 12;
                    let hour_portion = if hour_portion == 0 { 12 } else { hour_portion };
                    (hour_portion, Some(if hour < 12 { "AM" } else { "PM" }))
                }
                HourCycle::H23 => (hour, None),
                HourCycle::H24 => (if hour == 0 { 24 } else { hour }, None),
            };
            parts.push(("hour".to_owned(), format_hour(display_hour, hour_style)));
            if let Some(minute_style) = options.minute {
                parts.push(("literal".to_owned(), ":".to_owned()));
                parts.push(("minute".to_owned(), format_minute(minute, minute_style)));
            }
            if let Some(second_style) = options.second {
                parts.push(("literal".to_owned(), ":".to_owned()));
                parts.push(("second".to_owned(), format_minute(second, second_style)));
            }
            if let Some(period) = day_period {
                parts.push(("literal".to_owned(), " ".to_owned()));
                parts.push(("dayPeriod".to_owned(), period.to_owned()));
            }
        } else if options.minute.is_some() || options.second.is_some() {
            if let Some(minute_style) = options.minute {
                parts.push(("minute".to_owned(), format_minute(minute, minute_style)));
            }
            if let Some(second_style) = options.second {
                if options.minute.is_some() {
                    parts.push(("literal".to_owned(), ":".to_owned()));
                }
                parts.push(("second".to_owned(), format_minute(second, second_style)));
            }
        }
        if let Some(zone) = options.time_zone_name {
            if options.hour.is_some() || options.minute.is_some() || options.second.is_some() {
                parts.push(("literal".to_owned(), " ".to_owned()));
            }
            parts.push((
                "timeZoneName".to_owned(),
                match zone {
                    TimeZoneNameStyle::Short => "UTC".to_owned(),
                    TimeZoneNameStyle::Long => "Coordinated Universal Time".to_owned(),
                },
            ));
        }
        Ok(parts)
    }
}

impl Realm {
    /// Install the `Intl` namespace, its constructors and prototypes.
    pub(crate) fn install_intl(&mut self) {
        let object_prototype = self.object_prototype();
        let function_prototype = self.function_prototype();
        let namespace = self.create_object(Some(object_prototype));
        for kind in IntlKind::CLASSES {
            let prototype = self.create_object(Some(object_prototype));
            self.intl_prototypes.insert(kind, prototype);
            let constructor =
                self.allocate_host(Some(function_prototype), ObjectHost::IntlConstructor(kind));
            self.install_function_metadata(constructor, kind.name(), 0);
            self.define_property(
                constructor,
                "prototype",
                frozen_data(JsValue::Object(prototype)),
            );
            self.define_property(
                prototype,
                "constructor",
                builtin_data(JsValue::Object(constructor)),
            );
            let tag = format!("Intl.{}", kind.name());
            self.install_intl_tag(prototype, &tag);
            self.install_intl_static(
                constructor,
                IntlOp::SupportedLocalesOf,
                "supportedLocalesOf",
                1,
            );
            self.install_intl_members(kind, prototype);
            self.define_property(
                namespace,
                kind.name(),
                builtin_data(JsValue::Object(constructor)),
            );
        }
        for kind in [IntlKind::Segments, IntlKind::SegmentIterator] {
            let prototype = self.create_object(Some(object_prototype));
            self.intl_prototypes.insert(kind, prototype);
            self.install_intl_members(kind, prototype);
        }
        self.install_intl_static(
            namespace,
            IntlOp::GetCanonicalLocales,
            "getCanonicalLocales",
            1,
        );
        self.install_intl_tag(namespace, "Intl");
        let global = self.global_object();
        self.define_property(global, "Intl", builtin_data(JsValue::Object(namespace)));
    }

    fn install_intl_members(&mut self, kind: IntlKind, prototype: ObjectId) {
        match kind {
            IntlKind::Collator => {
                self.install_intl_getter(prototype, IntlOp::CollatorCompare, "compare");
                self.install_intl_method(prototype, IntlOp::ResolvedOptions, "resolvedOptions", 0);
            }
            IntlKind::DateTimeFormat => {
                self.install_intl_getter(prototype, IntlOp::DateTimeFormatFormat, "format");
                self.install_intl_method(
                    prototype,
                    IntlOp::DateTimeFormatFormatToParts,
                    "formatToParts",
                    1,
                );
                self.install_intl_method(prototype, IntlOp::ResolvedOptions, "resolvedOptions", 0);
            }
            IntlKind::NumberFormat => {
                self.install_intl_getter(prototype, IntlOp::NumberFormatFormat, "format");
                self.install_intl_method(
                    prototype,
                    IntlOp::NumberFormatFormatToParts,
                    "formatToParts",
                    1,
                );
                self.install_intl_method(prototype, IntlOp::ResolvedOptions, "resolvedOptions", 0);
            }
            IntlKind::PluralRules => {
                self.install_intl_method(prototype, IntlOp::PluralRulesSelect, "select", 1);
                self.install_intl_method(prototype, IntlOp::ResolvedOptions, "resolvedOptions", 0);
            }
            IntlKind::Segmenter => {
                self.install_intl_method(prototype, IntlOp::SegmenterSegment, "segment", 1);
                self.install_intl_method(prototype, IntlOp::ResolvedOptions, "resolvedOptions", 0);
            }
            IntlKind::Segments => {
                self.install_intl_method(prototype, IntlOp::SegmentsContaining, "containing", 1);
                self.install_intl_method(prototype, IntlOp::SegmentsToArray, "toArray", 0);
                let symbol = crate::JsSymbol::well_known("@@iterator");
                self.install_intl_symbol_method(
                    prototype,
                    &symbol,
                    IntlOp::SegmentsIterator,
                    "[Symbol.iterator]",
                );
            }
            IntlKind::SegmentIterator => {
                self.install_intl_method(prototype, IntlOp::SegmentIteratorNext, "next", 0);
                let symbol = crate::JsSymbol::well_known("@@iterator");
                self.install_intl_symbol_method(
                    prototype,
                    &symbol,
                    IntlOp::SegmentIteratorSelf,
                    "[Symbol.iterator]",
                );
            }
        }
    }

    fn install_intl_method(
        &mut self,
        holder: ObjectId,
        operation: IntlOp,
        name: &str,
        arity: usize,
    ) {
        let function_prototype = self.function_prototype();
        let function = self.allocate_host(
            Some(function_prototype),
            ObjectHost::NativeFunction(NativeFunction::Intl(operation)),
        );
        self.install_function_metadata(function, name, arity);
        self.define_property(holder, name, builtin_data(JsValue::Object(function)));
    }

    fn install_intl_static(
        &mut self,
        holder: ObjectId,
        operation: IntlOp,
        name: &str,
        arity: usize,
    ) {
        self.install_intl_method(holder, operation, name, arity);
    }

    fn install_intl_getter(&mut self, holder: ObjectId, operation: IntlOp, name: &str) {
        let function_prototype = self.function_prototype();
        let getter = self.allocate_host(
            Some(function_prototype),
            ObjectHost::NativeFunction(NativeFunction::Intl(operation)),
        );
        self.install_function_metadata(getter, &format!("get {name}"), 0);
        self.define_property(
            holder,
            name,
            PropertyDescriptor {
                value: JsValue::Undefined,
                writable: false,
                getter: Some(getter),
                setter: None,
                enumerable: false,
                configurable: true,
            },
        );
    }

    fn install_intl_symbol_method(
        &mut self,
        holder: ObjectId,
        symbol: &crate::JsSymbol,
        operation: IntlOp,
        name: &str,
    ) {
        let function_prototype = self.function_prototype();
        let function = self.allocate_host(
            Some(function_prototype),
            ObjectHost::NativeFunction(NativeFunction::Intl(operation)),
        );
        self.install_function_metadata(function, name, 0);
        self.define_symbol_property(
            holder,
            symbol,
            PropertyDescriptor {
                value: JsValue::Object(function),
                writable: true,
                getter: None,
                setter: None,
                enumerable: false,
                configurable: true,
            },
        );
    }

    fn install_intl_tag(&mut self, holder: ObjectId, tag: &str) {
        let symbol = crate::JsSymbol::well_known("@@toStringTag");
        self.define_symbol_property(
            holder,
            &symbol,
            PropertyDescriptor {
                value: JsValue::String(tag.to_owned().into()),
                writable: false,
                getter: None,
                setter: None,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// `%Intl.<kind>.prototype%`, the prototype of that class's instances.
    pub(crate) fn intl_prototype(&self, kind: IntlKind) -> ObjectId {
        self.intl_prototypes
            .get(&kind)
            .copied()
            .unwrap_or_else(|| self.object_prototype())
    }
}

/// A writable, configurable, non-enumerable data property.
fn builtin_data(value: JsValue) -> PropertyDescriptor {
    PropertyDescriptor {
        value,
        writable: true,
        getter: None,
        setter: None,
        enumerable: false,
        configurable: true,
    }
}

/// A read-only, non-configurable data property, such as a constructor's
/// `prototype`.
fn frozen_data(value: JsValue) -> PropertyDescriptor {
    PropertyDescriptor {
        value,
        writable: false,
        getter: None,
        setter: None,
        enumerable: false,
        configurable: false,
    }
}

const MONTHS_LONG: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_NARROW: [&str; 12] = ["J", "F", "M", "A", "M", "J", "J", "A", "S", "O", "N", "D"];
const WEEKDAYS_LONG: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const WEEKDAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const WEEKDAYS_NARROW: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];

fn numeric_style_name(style: NumericStyle) -> &'static str {
    match style {
        NumericStyle::Numeric => "numeric",
        NumericStyle::TwoDigit => "2-digit",
    }
}

fn month_style_name(style: MonthStyle) -> &'static str {
    match style {
        MonthStyle::Numeric => "numeric",
        MonthStyle::TwoDigit => "2-digit",
        MonthStyle::Long => "long",
        MonthStyle::Short => "short",
        MonthStyle::Narrow => "narrow",
    }
}

fn weekday_style_name(style: WeekdayStyle) -> &'static str {
    match style {
        WeekdayStyle::Long => "long",
        WeekdayStyle::Short => "short",
        WeekdayStyle::Narrow => "narrow",
    }
}

fn format_month_number(month: i32, style: MonthStyle) -> String {
    match style {
        MonthStyle::Numeric => month.to_string(),
        MonthStyle::TwoDigit => format!("{month:02}"),
        _ => month.to_string(),
    }
}

fn format_day_number(day: i32, style: NumericStyle) -> String {
    match style {
        NumericStyle::Numeric => day.to_string(),
        NumericStyle::TwoDigit => format!("{day:02}"),
    }
}

fn format_year_number(year: i32, style: NumericStyle) -> String {
    match style {
        NumericStyle::Numeric => year.to_string(),
        NumericStyle::TwoDigit => format!("{:02}", year.rem_euclid(100)),
    }
}

fn format_hour(hour: i32, style: NumericStyle) -> String {
    match style {
        NumericStyle::Numeric => hour.to_string(),
        NumericStyle::TwoDigit => format!("{hour:02}"),
    }
}

fn format_minute(minute: i32, _style: NumericStyle) -> String {
    format!("{minute:02}")
}

/// The en-US decimal/grouping separator and percent sign, as used here.
const GROUP_SEPARATOR: char = ',';
const DECIMAL_SEPARATOR: char = '.';

fn currency_digits(code: &str) -> u8 {
    match code {
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX"
        | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        _ => 2,
    }
}

fn currency_symbol(code: &str) -> &'static str {
    match code {
        "USD" => "$",
        "EUR" => "€",
        "GBP" => "£",
        "JPY" => "¥",
        "CNY" => "CN¥",
        "KRW" => "₩",
        "INR" => "₹",
        "RUB" => "₽",
        "BRL" => "R$",
        "CAD" => "CA$",
        "AUD" => "A$",
        "CHF" => "CHF",
        _ => "",
    }
}

fn number_format_parts(options: &NumberFormatOptions, value: f64) -> Vec<(String, String)> {
    if value.is_nan() {
        return vec![("nan".to_owned(), "NaN".to_owned())];
    }
    let negative = value.is_sign_negative();
    let mut parts = Vec::new();
    let sign_visible = match options.sign_display {
        SignDisplay::Auto | SignDisplay::Negative => negative,
        SignDisplay::Never => false,
        SignDisplay::Always => true,
        SignDisplay::ExceptZero => value != 0.0,
    };
    if sign_visible {
        parts.push((
            if negative { "minusSign" } else { "plusSign" }.to_owned(),
            if negative { "-" } else { "+" }.to_owned(),
        ));
    }
    if value.is_infinite() {
        parts.push(("infinity".to_owned(), "∞".to_owned()));
        return parts;
    }
    let mut magnitude = value.abs();
    if matches!(options.style, NumberStyle::Percent) {
        magnitude *= 100.0;
    }
    let (mut scaled, mut exponent) = if options.notation == NumberNotation::Compact {
        compact_scale(magnitude)
    } else {
        (magnitude, 0)
    };
    let (rounded, maximum_fraction) = loop {
        let maximum_fraction = if options.compact_default_rounding {
            usize::from(scaled < 10.0)
        } else {
            usize::from(options.maximum_fraction_digits)
        };
        let rounded = if maximum_fraction == 0 {
            scaled.round()
        } else {
            let factor = 10f64.powi(i32::try_from(maximum_fraction).unwrap_or(100));
            (scaled * factor).round() / factor
        };
        if options.notation == NumberNotation::Compact && rounded >= 1_000.0 && exponent < 4 {
            exponent += 1;
            scaled = magnitude / 1_000f64.powi(exponent);
            continue;
        }
        break (rounded, maximum_fraction);
    };
    let mut text = format!("{rounded:.maximum_fraction$}");
    if !text.contains('.') {
        text.push('.');
    }
    let (integer_text, fraction_text) = text.split_once('.').unwrap_or((&text, ""));
    let mut fraction = fraction_text.to_owned();
    while fraction.len() > usize::from(options.minimum_fraction_digits) && fraction.ends_with('0') {
        fraction.pop();
    }
    while fraction.len() < usize::from(options.minimum_fraction_digits) {
        fraction.push('0');
    }
    let mut integer = integer_text.trim_start_matches('0').to_owned();
    if integer.is_empty() {
        integer.push('0');
    }
    while integer.len() < usize::from(options.minimum_integer_digits) {
        integer.insert(0, '0');
    }
    if let NumberStyle::Currency(code) = &options.style {
        match options.currency_display {
            CurrencyDisplay::Symbol => {
                let symbol = currency_symbol(code);
                parts.push((
                    "currency".to_owned(),
                    if symbol.is_empty() {
                        format!("{code} ")
                    } else {
                        symbol.to_owned()
                    },
                ));
            }
            CurrencyDisplay::Code => {
                parts.push(("currency".to_owned(), format!("{code} ")));
            }
        }
    }
    push_integer_parts(
        &mut parts,
        &integer,
        options.use_grouping && options.notation == NumberNotation::Standard,
    );
    if !fraction.is_empty() {
        parts.push(("decimal".to_owned(), DECIMAL_SEPARATOR.to_string()));
        parts.push(("fraction".to_owned(), fraction));
    }
    if options.notation == NumberNotation::Compact && exponent > 0 {
        parts.push((
            "compact".to_owned(),
            ["", "K", "M", "B", "T"][usize::try_from(exponent).unwrap_or(0)].to_owned(),
        ));
    }
    if matches!(options.style, NumberStyle::Percent) {
        parts.push(("percentSign".to_owned(), "%".to_owned()));
    }
    parts
}

/// The compact divisor exponent (0 for values below 1000) and the value
/// divided by it. Above the largest built-in suffix the value keeps growing;
/// it is still exact, only the suffix stops changing.
fn compact_scale(magnitude: f64) -> (f64, i32) {
    if magnitude < 1_000.0 {
        return (magnitude, 0);
    }
    let exponent = (magnitude.log10() / 3.0).floor() as i32;
    let exponent = exponent.clamp(1, 4);
    (magnitude / 1_000f64.powi(exponent), exponent)
}

fn push_integer_parts(parts: &mut Vec<(String, String)>, integer: &str, grouping: bool) {
    if !grouping || integer.len() <= 3 {
        parts.push(("integer".to_owned(), integer.to_owned()));
        return;
    }
    let first = integer.len() % 3;
    let first = if first == 0 { 3 } else { first };
    parts.push(("integer".to_owned(), integer[..first].to_owned()));
    let mut index = first;
    while index < integer.len() {
        parts.push(("group".to_owned(), GROUP_SEPARATOR.to_string()));
        parts.push(("integer".to_owned(), integer[index..index + 3].to_owned()));
        index += 3;
    }
}

fn plural_category(options: &PluralRulesOptions, value: f64) -> &'static str {
    if !value.is_finite() {
        return "other";
    }
    let magnitude = value.abs();
    if options.ordinal {
        let integer = magnitude.trunc() as i64;
        let mod10 = integer % 10;
        let mod100 = integer % 100;
        if mod10 == 1 && mod100 != 11 {
            "one"
        } else if mod10 == 2 && mod100 != 12 {
            "two"
        } else if mod10 == 3 && mod100 != 13 {
            "few"
        } else {
            "other"
        }
    } else if magnitude == 1.0 {
        "one"
    } else {
        "other"
    }
}

fn segment_string(input: &JsString, granularity: Granularity) -> Vec<IntlSegment> {
    let text = input.to_lossy();
    let mut entries = Vec::new();
    let mut index = 0usize;
    match granularity {
        Granularity::Grapheme => {
            for grapheme in text.graphemes(true) {
                entries.push(IntlSegment {
                    segment: grapheme.into(),
                    index,
                    word_like: None,
                });
                index += grapheme.encode_utf16().count();
            }
        }
        Granularity::Word => {
            for (_, word) in text.split_word_bound_indices() {
                entries.push(IntlSegment {
                    segment: word.into(),
                    index,
                    word_like: Some(word.chars().any(char::is_alphanumeric)),
                });
                index += word.encode_utf16().count();
            }
        }
        Granularity::Sentence => {
            for (_, sentence) in text.split_sentence_bound_indices() {
                entries.push(IntlSegment {
                    segment: sentence.into(),
                    index,
                    word_like: None,
                });
                index += sentence.encode_utf16().count();
            }
        }
    }
    entries
}

/// Canonicalize one Unicode BCP 47 language tag. The parser is deliberately a
/// structural one: it rejects malformed subtags and canonicalizes case, but
/// does not carry the IANA registry's deprecated-tag replacements.
fn canonicalize_locale(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let mut subtags = raw.split('-');
    let language = subtags.next()?;
    if !(2..=8).contains(&language.len())
        || !language.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return None;
    }
    let mut output = language.to_ascii_lowercase();
    let rest: Vec<&str> = subtags.collect();
    let mut index = 0;
    if index < rest.len()
        && rest[index].len() == 4
        && rest[index].bytes().all(|b| b.is_ascii_alphabetic())
    {
        output.push('-');
        output.push_str(&title_case(rest[index]));
        index += 1;
    }
    if index < rest.len()
        && ((rest[index].len() == 2 && rest[index].bytes().all(|b| b.is_ascii_alphabetic()))
            || (rest[index].len() == 3 && rest[index].bytes().all(|b| b.is_ascii_digit())))
    {
        output.push('-');
        output.push_str(&rest[index].to_ascii_uppercase());
        index += 1;
    }
    let mut seen_extension = false;
    let mut singletons = Vec::new();
    while index < rest.len() {
        let tag = rest[index];
        if tag.len() == 1 {
            let singleton = tag.to_ascii_lowercase();
            if !tag.bytes().all(|byte| byte.is_ascii_alphanumeric())
                || singletons.contains(&singleton)
            {
                return None;
            }
            singletons.push(singleton.clone());
            output.push('-');
            output.push_str(&singleton);
            index += 1;
            if singleton == "x" {
                let mut count = 0;
                while index < rest.len() {
                    let private = rest[index];
                    if private.is_empty()
                        || private.len() > 8
                        || !private.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    {
                        return None;
                    }
                    output.push('-');
                    output.push_str(&private.to_ascii_lowercase());
                    index += 1;
                    count += 1;
                }
                if count == 0 {
                    return None;
                }
                break;
            }
            let start = index;
            while index < rest.len()
                && (2..=8).contains(&rest[index].len())
                && rest[index].bytes().all(|byte| byte.is_ascii_alphanumeric())
            {
                output.push('-');
                output.push_str(&rest[index].to_ascii_lowercase());
                index += 1;
            }
            if index == start {
                return None;
            }
            seen_extension = true;
        } else if !seen_extension && is_variant(tag) {
            output.push('-');
            output.push_str(&tag.to_ascii_lowercase());
            index += 1;
        } else {
            return None;
        }
    }
    Some(output)
}

fn is_variant(tag: &str) -> bool {
    ((5..=8).contains(&tag.len()) && tag.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        || (tag.len() == 4
            && tag.as_bytes()[0].is_ascii_digit()
            && tag.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

fn title_case(text: &str) -> String {
    let mut characters = text.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    let mut result = String::new();
    result.push(first.to_ascii_uppercase());
    result.extend(characters.map(|character| character.to_ascii_lowercase()));
    result
}

#[cfg(test)]
mod tests {
    use crate::runtime::JsRuntime;
    use render_html::parse_document;

    fn run(source: &str) -> Result<String, crate::JsError> {
        let mut parsed = parse_document("<!doctype html><html><body></body></html>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime.execute(&mut parsed.dom, source)?;
        assert!(runtime.prelude_error().is_none());
        Ok(outcome.value.to_js_string().to_lossy())
    }

    fn ok(source: &str) -> String {
        run(source).unwrap_or_else(|error| panic!("{source}\n=> {error}"))
    }

    fn error_name(source: &str) -> String {
        ok(&format!(
            "try {{ {source}; 'no error' }} catch (e) {{ e.name }}"
        ))
    }

    #[test]
    fn segmenter_word_boundaries_have_utf16_indexes_and_word_flags() {
        assert_eq!(
            ok(
                "new Intl.Segmenter('en',{granularity:'word'}).segment('a b').toArray().map(function(s){return s.segment+':'+s.index+':'+(s.isWordLike?1:0);}).join(',')"
            ),
            "a:0:1, :1:0,b:2:1"
        );
        assert_eq!(
            ok(
                "Array.from(new Intl.Segmenter('en',{granularity:'word'}).segment('hi, there!')).map(function(s){return s.segment;}).join('|')"
            ),
            "hi|,| |there|!"
        );
        // A lone surrogate is still one UTF-16 unit, so the index of the next
        // segment does not shift when the host string is made well formed.
        assert_eq!(
            ok(
                "new Intl.Segmenter('en',{granularity:'grapheme'}).segment('\\uD83D\\uDE00x').toArray()[1].index"
            ),
            "2"
        );
    }

    #[test]
    fn segmenter_supports_graphemes_sentences_and_containing() {
        assert_eq!(
            ok(
                "new Intl.Segmenter('en').segment('a\\u0301b').toArray().map(function(s){return s.segment.length;}).join(',')"
            ),
            "2,1"
        );
        assert_eq!(
            ok(
                "new Intl.Segmenter('en',{granularity:'sentence'}).segment('One. Two!').toArray().length"
            ),
            "2"
        );
        assert_eq!(
            ok(
                "new Intl.Segmenter('en',{granularity:'word'}).segment('one two').containing(5).segment"
            ),
            "two"
        );
        assert_eq!(
            ok("new Intl.Segmenter('en').segment('abc').containing(3) === undefined"),
            "true"
        );
        assert_eq!(
            ok("new Intl.Segmenter('en').segment('abc').containing().segment"),
            "a"
        );
    }

    #[test]
    fn the_x_startup_pattern_constructs_without_arguments() {
        assert_eq!(
            ok("var s = new Intl.Segmenter; s.segment('abc').containing(1).segment"),
            "b"
        );
        assert_eq!(ok("typeof Intl.Segmenter"), "function");
    }

    #[test]
    fn segmenter_rejects_bad_options_and_requires_new() {
        assert_eq!(
            error_name("new Intl.Segmenter('en',{granularity:'bogus'})"),
            "RangeError"
        );
        assert_eq!(error_name("Intl.Segmenter()"), "TypeError");
    }

    #[test]
    fn number_format_covers_decimal_percent_currency_and_parts() {
        assert_eq!(
            ok("new Intl.NumberFormat('en-US').format(1234.5)"),
            "1,234.5"
        );
        assert_eq!(
            ok("new Intl.NumberFormat('en-US').format(-1234.5)"),
            "-1,234.5"
        );
        assert_eq!(ok("new Intl.NumberFormat('en-US').format(0)"), "0");
        assert_eq!(
            ok("new Intl.NumberFormat('en-US').format(1e21)"),
            "1,000,000,000,000,000,000,000"
        );
        assert_eq!(
            ok("new Intl.NumberFormat('en-US',{useGrouping:false}).format(1234.5)"),
            "1234.5"
        );
        assert_eq!(
            ok(
                "new Intl.NumberFormat('en-US',{minimumFractionDigits:2,maximumFractionDigits:2}).format(1)"
            ),
            "1.00"
        );
        assert_eq!(
            ok("new Intl.NumberFormat('en-US',{style:'percent'}).format(0.25)"),
            "25%"
        );
        assert_eq!(
            ok("new Intl.NumberFormat('en-US',{style:'currency',currency:'USD'}).format(1234.5)"),
            "$1,234.50"
        );
        assert_eq!(
            ok("new Intl.NumberFormat('en-US',{style:'currency',currency:'JPY'}).format(1234.5)"),
            "¥1,235"
        );
        assert_eq!(
            ok(
                "new Intl.NumberFormat('en-US',{style:'currency',currency:'USD',currencyDisplay:'code'}).format(1)"
            ),
            "USD 1.00"
        );
        assert_eq!(
            ok(
                "new Intl.NumberFormat('en-US').formatToParts(1234.5).map(function(p){return p.type+':'+p.value;}).join('|')"
            ),
            "integer:1|group:,|integer:234|decimal:.|fraction:5"
        );
        assert_eq!(
            ok(
                "[1000,1200,12345,123456,1234567,12345678].map(function(n){return new Intl.NumberFormat('en-US',{notation:'compact'}).format(n);}).join(',')"
            ),
            "1K,1.2K,12K,123K,1.2M,12M"
        );
        assert_eq!(
            ok(
                "new Intl.NumberFormat('en-US',{style:'currency',currency:'USD',notation:'compact',maximumFractionDigits:1}).format(1500000)"
            ),
            "$1.5M"
        );
        assert_eq!(
            ok(
                "new Intl.NumberFormat('en-US',{notation:'compact'}).formatToParts(1200).map(function(p){return p.type+':'+p.value;}).join('|')"
            ),
            "integer:1|decimal:.|fraction:2|compact:K"
        );
        assert_eq!(
            ok("var f = new Intl.NumberFormat('en-US').format; f(1234.5)"),
            "1,234.5"
        );
        assert_eq!(ok("Intl.NumberFormat('en-US').format(2)"), "2");
    }

    #[test]
    fn number_format_rejects_missing_currency_and_unsupported_notation() {
        assert_eq!(
            error_name("new Intl.NumberFormat('en-US',{style:'currency'})"),
            "RangeError"
        );
        assert_eq!(
            error_name("new Intl.NumberFormat('en-US',{notation:'scientific'})"),
            "RangeError"
        );
        assert_eq!(
            error_name("new Intl.NumberFormat('en-US',{style:'percent',currency:'USD'})"),
            "TypeError"
        );
    }

    #[test]
    fn date_time_format_uses_en_us_fields_and_utc() {
        assert_eq!(
            ok(
                "new Intl.DateTimeFormat('en-US',{timeZone:'UTC',year:'numeric',month:'long',day:'numeric'}).format(new Date(Date.UTC(2026,10,2)))"
            ),
            "November 2, 2026"
        );
        assert_eq!(
            ok(
                "new Intl.DateTimeFormat('en-US',{timeZone:'UTC'}).format(new Date(Date.UTC(2026,10,2)))"
            ),
            "11/2/2026"
        );
        assert_eq!(
            ok(
                "new Intl.DateTimeFormat('en-US',{timeZone:'UTC',hour:'numeric',minute:'2-digit',hour12:true}).format(new Date(Date.UTC(2026,10,2,15,5)))"
            ),
            "3:05 PM"
        );
        assert_eq!(
            ok(
                "new Intl.DateTimeFormat('en-US',{timeZone:'UTC',hour:'numeric',minute:'2-digit',hour12:false}).format(new Date(Date.UTC(2026,10,2,15,5)))"
            ),
            "15:05"
        );
        assert_eq!(
            ok(
                "new Intl.DateTimeFormat('en-US',{timeZone:'UTC',hour:'numeric',minute:'2-digit',hourCycle:'h11'}).format(new Date(Date.UTC(2026,10,2,0,5)))"
            ),
            "0:05 AM"
        );
        assert_eq!(
            ok(
                "new Intl.DateTimeFormat('en-US',{timeZone:'UTC',weekday:'long',year:'numeric',month:'long',day:'numeric'}).format(new Date(Date.UTC(2026,10,2)))"
            ),
            "Monday, November 2, 2026"
        );
        assert_eq!(
            ok(
                "new Intl.DateTimeFormat('en-US',{timeZone:'UTC',dateStyle:'short'}).format(new Date(Date.UTC(2026,10,2)))"
            ),
            "11/2/26"
        );
        assert_eq!(
            ok(
                "new Intl.DateTimeFormat('en-US',{timeZone:'UTC',hour:'2-digit',minute:'2-digit'}).formatToParts(new Date(Date.UTC(2026,10,2,15,5))).map(function(p){return p.type;}).join(',')"
            ),
            "hour,literal,minute,literal,dayPeriod"
        );
        assert_eq!(
            ok("new Intl.DateTimeFormat('en-US',{timeZone:'UTC'}).resolvedOptions().timeZone"),
            "UTC"
        );
        assert_eq!(
            ok(
                "var f = new Intl.DateTimeFormat('en-US',{timeZone:'UTC',year:'numeric'}).format; f(new Date(Date.UTC(2026,10,2)))"
            ),
            "2026"
        );
    }

    #[test]
    fn date_time_format_rejects_unknown_time_zones_and_invalid_dates() {
        assert_eq!(
            error_name("new Intl.DateTimeFormat('en-US',{timeZone:'America/New_York'})"),
            "RangeError"
        );
        assert_eq!(
            error_name("new Intl.DateTimeFormat('en-US').format(new Date(NaN))"),
            "RangeError"
        );
        assert_eq!(
            error_name("new Intl.DateTimeFormat('en-US',{dateStyle:'full',year:'numeric'})"),
            "TypeError"
        );
    }

    #[test]
    fn plural_rules_cardinal_and_ordinal_english() {
        assert_eq!(ok("new Intl.PluralRules('en').select(1)"), "one");
        assert_eq!(ok("new Intl.PluralRules('en').select(2)"), "other");
        assert_eq!(ok("new Intl.PluralRules('en').select(1.5)"), "other");
        assert_eq!(
            ok(
                "[1,2,3,4,11,21,22,23].map(function(n){return new Intl.PluralRules('en',{type:'ordinal'}).select(n);}).join(',')"
            ),
            "one,two,few,other,other,one,two,few"
        );
        assert_eq!(
            ok("new Intl.PluralRules('en').resolvedOptions().pluralCategories.join(',')"),
            "one,other"
        );
    }

    #[test]
    fn collator_and_canonical_locales_are_real_but_locale_limited() {
        assert_eq!(
            ok(
                "[new Intl.Collator('en').compare('a','b'), new Intl.Collator('en').compare('b','a'), new Intl.Collator('en').compare('a','a')].join(',')"
            ),
            "-1,1,0"
        );
        assert_eq!(
            ok("var c = new Intl.Collator('en').compare; c('a','b')"),
            "-1"
        );
        assert_eq!(
            ok("Intl.getCanonicalLocales(['EN-us','en-US','zh-hant-tw']).join(',')"),
            "en-US,zh-Hant-TW"
        );
        assert_eq!(
            ok("Intl.NumberFormat.supportedLocalesOf(['en-GB','fr','en-US']).join(',')"),
            "en-GB,en-US"
        );
        assert_eq!(
            error_name("Intl.getCanonicalLocales('not a locale')"),
            "RangeError"
        );
    }

    #[test]
    fn intl_instances_have_their_interface_tags() {
        assert_eq!(
            ok("Object.prototype.toString.call(new Intl.NumberFormat())"),
            "[object Intl.NumberFormat]"
        );
        assert_eq!(
            ok("Object.prototype.toString.call(new Intl.Segmenter().segment('a'))"),
            "[object Object]"
        );
    }
}
