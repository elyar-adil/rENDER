//! The `Temporal` namespace (TC39 Temporal proposal), restricted to the ISO
//! 8601 calendar and to UTC. A `Temporal.ZonedDateTime` and every operation
//! that names a time zone are outside this module, and so is every calendar
//! other than `"iso8601"`.
//!
//! Each class is one file that owns its constructor, static functions,
//! getters and prototype methods, described by a [`Surface`] table. Instances
//! keep their internal slots in [`ObjectHost::Temporal`], so a method reads
//! its receiver through the same host that the constructor wrote. The shared
//! pieces are the value types, the argument conversions in [`options`] and
//! the ISO arithmetic in [`iso`].

pub(crate) mod duration;
pub(crate) mod iso;

mod format;
mod instant;
mod now;
mod options;
mod parse;
mod plain_date;
mod plain_date_time;
mod plain_month_day;
mod plain_time;
mod plain_year_month;
mod rounding;

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::PropertyDescriptor;
use crate::Realm;
use crate::runtime::JsRuntime;
use crate::value::ObjectHost;
use render_dom::Dom;

use duration::DurationRecord;
use iso::{IsoDate, IsoDateTime, IsoTime};

/// The classes and the namespace object that owns the `Now` functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TemporalKind {
    Duration,
    Instant,
    PlainDate,
    PlainDateTime,
    PlainTime,
    PlainYearMonth,
    PlainMonthDay,
    Now,
}

impl TemporalKind {
    /// The classes that have instances, and so a prototype.
    pub(crate) const CLASSES: [Self; 7] = [
        Self::Duration,
        Self::Instant,
        Self::PlainDate,
        Self::PlainDateTime,
        Self::PlainTime,
        Self::PlainYearMonth,
        Self::PlainMonthDay,
    ];

    /// The name the class is installed under on the `Temporal` namespace.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Duration => "Duration",
            Self::Instant => "Instant",
            Self::PlainDate => "PlainDate",
            Self::PlainDateTime => "PlainDateTime",
            Self::PlainTime => "PlainTime",
            Self::PlainYearMonth => "PlainYearMonth",
            Self::PlainMonthDay => "PlainMonthDay",
            Self::Now => "Now",
        }
    }

    const fn surface(self) -> Surface {
        match self {
            Self::Duration => duration::SURFACE,
            Self::Instant => instant::SURFACE,
            Self::PlainDate => plain_date::SURFACE,
            Self::PlainDateTime => plain_date_time::SURFACE,
            Self::PlainTime => plain_time::SURFACE,
            Self::PlainYearMonth => plain_year_month::SURFACE,
            Self::PlainMonthDay => plain_month_day::SURFACE,
            Self::Now => now::SURFACE,
        }
    }
}

/// Every operation a Temporal built-in can dispatch to. A name shared by
/// several classes (`add`, `round`, `toString`) is one variant, and the
/// class in [`NativeFunction::Temporal`] says which behaviour runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TemporalMethod {
    From,
    Compare,
    FromEpochMilliseconds,
    FromEpochNanoseconds,
    NowInstant,
    NowPlainDateIso,
    NowPlainDateTimeIso,
    NowPlainTimeIso,
    NowTimeZoneId,
    Years,
    Months,
    Weeks,
    Days,
    Hours,
    Minutes,
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
    Sign,
    Blank,
    EpochMilliseconds,
    EpochNanoseconds,
    CalendarId,
    Era,
    EraYear,
    Year,
    Month,
    MonthCode,
    Day,
    DayOfWeek,
    DayOfYear,
    WeekOfYear,
    YearOfWeek,
    DaysInWeek,
    DaysInMonth,
    DaysInYear,
    MonthsInYear,
    InLeapYear,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
    Add,
    Subtract,
    Until,
    Since,
    Round,
    Total,
    With,
    WithPlainTime,
    WithCalendar,
    Negated,
    Abs,
    Equals,
    ToString,
    ToJSON,
    ToLocaleString,
    ValueOf,
    ToPlainDate,
    ToPlainTime,
    ToPlainDateTime,
    ToPlainYearMonth,
    ToPlainMonthDay,
}

/// The internal slots of one Temporal instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TemporalValue {
    Duration(DurationRecord),
    /// Epoch nanoseconds, always within [`iso::MAX_INSTANT_NANOS`].
    Instant(i128),
    PlainDate(IsoDate),
    PlainDateTime(IsoDateTime),
    PlainTime(IsoTime),
    /// The ISO year and month, with the reference day the spec keeps for it.
    PlainYearMonth(IsoDate),
    /// The month and day, with the reference ISO year the spec keeps for it.
    PlainMonthDay(IsoDate),
}

/// A `(name, operation, length)` method entry.
pub(super) type Member = (&'static str, TemporalMethod, usize);

/// One method entry of a [`Surface`] table.
pub(super) const fn member(name: &'static str, method: TemporalMethod, length: usize) -> Member {
    (name, method, length)
}

/// The members one class installs. A class without a constructor (`Now`)
/// leaves `constructor_length` at `None`.
pub(super) struct Surface {
    pub(super) constructor_length: Option<usize>,
    pub(super) statics: &'static [Member],
    pub(super) methods: &'static [Member],
    pub(super) getters: &'static [(&'static str, TemporalMethod)],
}

/// The argument at `index`, or `undefined` when it is absent.
pub(super) fn argument(arguments: &[JsValue], index: usize) -> JsValue {
    arguments.get(index).cloned().unwrap_or(JsValue::Undefined)
}

/// Every object among `receiver` and `arguments`, pinned while a conversion
/// runs user code.
fn object_roots(receiver: Option<ObjectId>, arguments: &[JsValue]) -> Vec<ObjectId> {
    receiver
        .into_iter()
        .chain(arguments.iter().filter_map(|value| match value {
            JsValue::Object(object) => Some(*object),
            _ => None,
        }))
        .collect()
}

impl JsRuntime {
    /// Run one Temporal method or static function. The receiver and every
    /// object argument are pinned for the call, because a conversion may run
    /// user code that triggers a collection.
    pub(in crate::runtime) fn dispatch_temporal_native(
        &mut self,
        dom: &mut Dom,
        kind: TemporalKind,
        method: TemporalMethod,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let roots = object_roots(Some(receiver), arguments);
        self.with_transient_roots(&roots, |runtime| match kind {
            TemporalKind::Duration => runtime.duration_native(dom, method, receiver, arguments),
            TemporalKind::Instant => runtime.instant_native(dom, method, receiver, arguments),
            TemporalKind::PlainDate => runtime.plain_date_native(dom, method, receiver, arguments),
            TemporalKind::PlainDateTime => {
                runtime.plain_date_time_native(dom, method, receiver, arguments)
            }
            TemporalKind::PlainTime => runtime.plain_time_native(dom, method, receiver, arguments),
            TemporalKind::PlainYearMonth => {
                runtime.plain_year_month_native(dom, method, receiver, arguments)
            }
            TemporalKind::PlainMonthDay => {
                runtime.plain_month_day_native(dom, method, receiver, arguments)
            }
            TemporalKind::Now => runtime.now_native(dom, method, arguments),
        })
    }

    /// `new Temporal.<kind>(...)`, with `new_target` supplying the prototype.
    pub(in crate::runtime) fn temporal_construct(
        &mut self,
        dom: &mut Dom,
        kind: TemporalKind,
        new_target: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let roots = object_roots(None, arguments);
        self.with_transient_roots(&roots, |runtime| match kind {
            TemporalKind::Duration => runtime.duration_construct(dom, new_target, arguments),
            TemporalKind::Instant => runtime.instant_construct(dom, new_target, arguments),
            TemporalKind::PlainDate => runtime.plain_date_construct(dom, new_target, arguments),
            TemporalKind::PlainDateTime => {
                runtime.plain_date_time_construct(dom, new_target, arguments)
            }
            TemporalKind::PlainTime => runtime.plain_time_construct(dom, new_target, arguments),
            TemporalKind::PlainYearMonth => {
                runtime.plain_year_month_construct(dom, new_target, arguments)
            }
            TemporalKind::PlainMonthDay => {
                runtime.plain_month_day_construct(dom, new_target, arguments)
            }
            TemporalKind::Now => Err(JsError::type_error("Temporal.Now is not a constructor")),
        })
    }

    /// The slots of `receiver` if it is a Temporal instance of any class.
    pub(super) fn temporal_host(&self, receiver: ObjectId) -> Option<TemporalValue> {
        match self.realm.host(receiver) {
            Some(ObjectHost::Temporal(value)) => Some(value),
            _ => None,
        }
    }

    /// A new instance of `kind` with `value` as its slots, from the intrinsic
    /// prototype (never from `this`, so subclass receivers do not leak in).
    pub(super) fn temporal_instance(
        &mut self,
        kind: TemporalKind,
        value: TemporalValue,
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let prototype = self.realm.temporal_prototype(kind);
        Ok(JsValue::Object(self.realm.allocate_host(
            Some(prototype),
            ObjectHost::Temporal(value),
        )))
    }

    /// Route a constructor call through the class's own `prototype`, as
    /// `OrdinaryCreateFromConstructor` does.
    pub(super) fn temporal_prototype_for(
        &mut self,
        dom: &mut Dom,
        kind: TemporalKind,
        new_target: &JsValue,
    ) -> Result<ObjectId, JsError> {
        let fallback = self.realm.temporal_prototype(kind);
        self.prototype_from_new_target(dom, new_target, fallback)
    }
}

impl Realm {
    /// Install the `Temporal` namespace object and every class on `self`.
    pub(crate) fn install_temporal(&mut self) {
        let object_prototype = self.object_prototype();
        let function_prototype = self.function_prototype();
        let namespace = self.create_object(Some(object_prototype));
        let global = self.global_object();
        for kind in TemporalKind::CLASSES
            .into_iter()
            .chain(std::iter::once(TemporalKind::Now))
        {
            let surface = kind.surface();
            let holder = if let Some(length) = surface.constructor_length {
                let prototype = self.create_object(Some(object_prototype));
                let constructor = self.allocate_host(
                    Some(function_prototype),
                    ObjectHost::TemporalConstructor(kind),
                );
                self.install_function_metadata(constructor, kind.name(), length);
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
                self.install_temporal_tag(prototype, &format!("Temporal.{}", kind.name()));
                self.temporal_prototypes.insert(kind, prototype);
                for &(name, method, arity) in surface.statics {
                    self.install_temporal_method(constructor, kind, method, name, arity);
                }
                for &(name, method, arity) in surface.methods {
                    self.install_temporal_method(prototype, kind, method, name, arity);
                }
                for &(name, method) in surface.getters {
                    self.install_temporal_getter(prototype, kind, method, name);
                }
                constructor
            } else {
                let now = self.create_object(Some(object_prototype));
                for &(name, method, arity) in surface.methods {
                    self.install_temporal_method(now, kind, method, name, arity);
                }
                self.install_temporal_tag(now, "Temporal.Now");
                now
            };
            self.define_property(
                namespace,
                kind.name(),
                builtin_data(JsValue::Object(holder)),
            );
        }
        self.install_temporal_tag(namespace, "Temporal");
        self.define_property(global, "Temporal", builtin_data(JsValue::Object(namespace)));
    }

    fn install_temporal_method(
        &mut self,
        holder: ObjectId,
        kind: TemporalKind,
        method: TemporalMethod,
        name: &str,
        arity: usize,
    ) {
        let function_prototype = self.function_prototype();
        let function = self.allocate_host(
            Some(function_prototype),
            ObjectHost::NativeFunction(crate::value::NativeFunction::Temporal(kind, method)),
        );
        self.install_function_metadata(function, name, arity);
        self.define_property(holder, name, builtin_data(JsValue::Object(function)));
    }

    fn install_temporal_getter(
        &mut self,
        holder: ObjectId,
        kind: TemporalKind,
        method: TemporalMethod,
        name: &str,
    ) {
        let function_prototype = self.function_prototype();
        let getter = self.allocate_host(
            Some(function_prototype),
            ObjectHost::NativeFunction(crate::value::NativeFunction::Temporal(kind, method)),
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

    fn install_temporal_tag(&mut self, holder: ObjectId, tag: &str) {
        let symbol = crate::JsSymbol::well_known("@@toStringTag");
        self.define_symbol_property(
            holder,
            &symbol,
            PropertyDescriptor {
                value: JsValue::String(tag.to_owned()),
                writable: false,
                getter: None,
                setter: None,
                enumerable: false,
                configurable: true,
            },
        );
    }
}

/// A writable, configurable, non-enumerable data property: the shape of every
/// built-in method and of the namespace's members.
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
