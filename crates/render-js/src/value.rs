//! JavaScript values and the realm-owned object arena.

use std::collections::BTreeMap;
use std::fmt;
use std::num::FpCategory;

use crate::JsError;
use render_dom::NodeId;
use url::Url;

use crate::bigint::JsBigInt;
use crate::parser::FunctionKind;
use crate::utf16;

/// Stable identity for an object allocated in a [`Realm`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(usize);

impl ObjectId {
    /// Return the object's arena index. This is useful for diagnostics only.
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0
    }

    /// Rebuild an id from [`Self::as_usize`] output, for host-state
    /// bookkeeping that scans the realm arena.
    #[must_use]
    pub(crate) const fn from_index(index: usize) -> Self {
        Self(index)
    }
}

/// Values supported by the initial interpreter vertical slice.
#[derive(Clone, Debug, PartialEq)]
pub enum JsValue {
    Undefined,
    Null,
    Boolean(bool),
    Number(f64),
    /// The `BigInt` primitive (ECMA-262 6.1.6.2): an exact integer.
    BigInt(JsBigInt),
    String(String),
    Symbol(JsSymbol),
    Object(ObjectId),
}

/// A JavaScript symbol primitive. The description text rides inline so
/// string conversion stays a pure operation; identity compares `id` alone,
/// and the description is a function of the id for any given symbol.
#[derive(Clone, Debug, PartialEq)]
pub struct JsSymbol {
    id: u64,
    description: Option<String>,
}

/// Symbol ids below this constant are reserved for the well-known symbols
/// installed during realm bootstrap; runtime symbols start above it.
pub(crate) const FIRST_DYNAMIC_SYMBOL_ID: u64 = 1_000;

impl JsSymbol {
    pub(crate) const fn new(id: u64, description: Option<String>) -> Self {
        Self { id, description }
    }

    /// The well-known symbol whose registry key is `name` ("@@iterator",
    /// "@@toStringTag", ...). Descriptions mirror the registry keys, except the
    /// explicit-resource-management pair, which carries the specification's
    /// `Symbol.dispose` and `Symbol.asyncDispose` names because `toString()` shows them.
    #[must_use]
    pub(crate) fn well_known(name: &str) -> Self {
        match name {
            "@@iterator" => Self::new(1, Some("@@iterator".to_owned())),
            "@@asyncIterator" => Self::new(2, Some("@@asyncIterator".to_owned())),
            "@@toStringTag" => Self::new(3, Some("@@toStringTag".to_owned())),
            "@@toPrimitive" => Self::new(4, Some("@@toPrimitive".to_owned())),
            "@@hasInstance" => Self::new(5, Some("@@hasInstance".to_owned())),
            "@@species" => Self::new(6, Some("@@species".to_owned())),
            "@@isConcatSpreadable" => Self::new(7, Some("@@isConcatSpreadable".to_owned())),
            "@@unscopables" => Self::new(8, Some("@@unscopables".to_owned())),
            "@@match" => Self::new(9, Some("@@match".to_owned())),
            "@@matchAll" => Self::new(10, Some("@@matchAll".to_owned())),
            "@@replace" => Self::new(11, Some("@@replace".to_owned())),
            "@@search" => Self::new(12, Some("@@search".to_owned())),
            "@@split" => Self::new(13, Some("@@split".to_owned())),
            "@@dispose" => Self::new(14, Some("Symbol.dispose".to_owned())),
            "@@asyncDispose" => Self::new(15, Some("Symbol.asyncDispose".to_owned())),
            _ => Self::new(0, None),
        }
    }

    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The canonical string form (`Symbol(description)` / `Symbol()`).
    #[must_use]
    pub fn to_display(&self) -> String {
        match &self.description {
            Some(description) => format!("Symbol({description})"),
            None => "Symbol()".to_owned(),
        }
    }
}

impl JsValue {
    /// Apply the string conversion needed by the initial DOM bindings.
    #[must_use]
    pub fn to_js_string(&self) -> String {
        match self {
            Self::Undefined => "undefined".to_owned(),
            Self::Null => "null".to_owned(),
            Self::Boolean(value) => value.to_string(),
            Self::Number(value) => number_to_string(*value),
            Self::BigInt(value) => value.to_string_radix(10),
            Self::String(value) => value.clone(),
            Self::Symbol(symbol) => symbol.to_display(),
            Self::Object(_) => "[object Object]".to_owned(),
        }
    }
}

/// ECMA-262 `ToString(Number)`: shortest round-trip digits with decimal
/// notation for `1e-6 <= |x| < 1e21` and exponential notation outside that
/// range (`1e+21`, `1.5e-7`).
#[allow(
    clippy::cast_sign_loss,
    reason = "the digit-count and exponent branches guarantee non-negative values"
)]
pub(crate) fn number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_positive() {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        };
    }
    if value.classify() == FpCategory::Zero {
        return "0".to_owned();
    }
    let negative = value < 0.0;
    // `{:e}` renders the shortest round-trip mantissa, e.g. `1.5e-7`.
    let formatted = format!("{:e}", value.abs());
    let (mantissa, exponent) = formatted
        .split_once('e')
        .expect("LowerExp always emits an exponent");
    let exponent: i32 = exponent.parse().expect("LowerExp emits a decimal exponent");
    let digits: String = mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "shortest round-trip digit counts stay far below i32::MAX"
    )]
    let k = digits.len() as i32;
    let n = exponent + 1;
    let text = if k <= n && n <= 21 {
        let mut text = digits;
        text.push_str(&"0".repeat((n - k) as usize));
        text
    } else if 0 < n && n <= 21 {
        let mut text = digits;
        text.insert(n as usize, '.');
        text
    } else if -6 < n && n <= 0 {
        format!("0.{}{}", "0".repeat((-n) as usize), digits)
    } else {
        let exponent = n - 1;
        let sign = if exponent < 0 { "-" } else { "+" };
        let magnitude = exponent.unsigned_abs();
        if k == 1 {
            format!("{digits}e{sign}{magnitude}")
        } else {
            format!("{}.{}e{sign}{magnitude}", &digits[..1], &digits[1..])
        }
    };
    if negative { format!("-{text}") } else { text }
}

pub(crate) fn location_components(url: &Url) -> [(&'static str, String); 9] {
    let hostname = url.host_str().unwrap_or_default().to_owned();
    let port = url.port().map_or_else(String::new, |port| port.to_string());
    let host = if port.is_empty() {
        hostname.clone()
    } else {
        format!("{hostname}:{port}")
    };
    [
        ("href", url.as_str().to_owned()),
        ("origin", url.origin().ascii_serialization()),
        ("protocol", format!("{}:", url.scheme())),
        ("host", host),
        ("hostname", hostname),
        ("port", port),
        ("pathname", url.path().to_owned()),
        (
            "search",
            url.query()
                .map_or_else(String::new, |query| format!("?{query}")),
        ),
        (
            "hash",
            url.fragment()
                .map_or_else(String::new, |fragment| format!("#{fragment}")),
        ),
    ]
}

/// An own property descriptor: either a data property (value slot) or an
/// accessor property (getter/setter function objects).
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyDescriptor {
    pub value: JsValue,
    pub writable: bool,
    pub getter: Option<ObjectId>,
    pub setter: Option<ObjectId>,
    pub enumerable: bool,
    pub configurable: bool,
}

impl PropertyDescriptor {
    #[must_use]
    pub const fn data(value: JsValue) -> Self {
        Self {
            value,
            writable: true,
            getter: None,
            setter: None,
            enumerable: true,
            configurable: true,
        }
    }

    const fn builtin(value: JsValue) -> Self {
        Self {
            value,
            writable: true,
            getter: None,
            setter: None,
            enumerable: false,
            configurable: true,
        }
    }

    /// Accessor properties carry function objects in the getter/setter
    /// slots and never use the value slot.
    #[must_use]
    pub const fn is_accessor(&self) -> bool {
        self.getter.is_some() || self.setter.is_some()
    }
}

/// ECMAScript `SameValue` for two JavaScript values: `NaN` equals itself and
/// `+0` differs from `-0`.
#[allow(
    clippy::float_cmp,
    reason = "SameValue compares IEEE values bit-for-bit by definition"
)]
pub(crate) fn same_value(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        // ECMA-262 7.2.11 SameValue tells `+0` from `-0`.
        (JsValue::Number(left), JsValue::Number(right)) => {
            (left.is_nan() && right.is_nan())
                || (left == right && left.is_sign_negative() == right.is_sign_negative())
        }
        _ => left == right,
    }
}

/// ECMA-262 `CanonicalNumericIndexString` (§7.1.21): `-0`, and any string that
/// `ToString` of its own `ToNumber` reproduces exactly (so `"1.5"` qualifies and
/// `"1.50"` does not). Returns the number the key names.
pub(crate) fn canonical_numeric_key(key: &str) -> Option<f64> {
    if key == "-0" {
        return Some(-0.0);
    }
    let number: f64 = key.parse().ok()?;
    (JsValue::Number(number).to_js_string() == key).then_some(number)
}

/// ECMA-262 `ValidateAndApplyPropertyDescriptor` (§10.1.6.3) for a property
/// that is not configurable. The redefinition must stay non-configurable and
/// keep enumerability and kind. An accessor keeps its functions. A data
/// property that is not writable keeps its value and stays non-writable, and a
/// writable one may take a new value or lose writability.
fn non_configurable_redefinition_allowed(
    current: &PropertyDescriptor,
    next: &PropertyDescriptor,
) -> bool {
    if next.configurable
        || next.enumerable != current.enumerable
        || next.is_accessor() != current.is_accessor()
    {
        return false;
    }
    if current.is_accessor() {
        return next.getter == current.getter && next.setter == current.setter;
    }
    if current.writable {
        return true;
    }
    !next.writable && same_value(&current.value, &next.value)
}

/// ECMA-262 §6.1.7 `CanonicalNumericIndexString`: a property key is a
/// canonical numeric index string when it is the shortest decimal form of a
/// non-negative integer below 2^32-1.
fn is_canonical_index(key: &str) -> bool {
    key.parse::<u32>()
        .ok()
        .is_some_and(|index| index.to_string() == key)
}

/// ECMA-262 §10.4.3.1 `StringGetOwnProperty`: the own slots a String wrapper
/// exposes for the primitive it hosts. Indexed characters are writable:false,
/// enumerable:true, configurable:false; `length` is writable:false,
/// enumerable:false, configurable:false. Ordinary properties are unaffected, so
/// this is only consulted for keys the object does not already carry.
fn string_exotic_property(text: &str, key: &str) -> Option<PropertyDescriptor> {
    if key == "length" {
        #[allow(
            clippy::cast_precision_loss,
            reason = "string lengths stay far below any precision boundary"
        )]
        return Some(PropertyDescriptor {
            value: JsValue::Number(utf16::utf16_length(text) as f64),
            writable: false,
            getter: None,
            setter: None,
            enumerable: false,
            configurable: false,
        });
    }
    let index: u32 = key.parse().ok()?;
    if index.to_string() != key {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the canonical index form bounds the value at u32::MAX"
    )]
    let unit = utf16::utf16_units(text).get(index as usize).copied()?;
    Some(PropertyDescriptor {
        value: JsValue::String(utf16::string_from_unit(unit)),
        writable: false,
        getter: None,
        setter: None,
        enumerable: true,
        configurable: false,
    })
}

/// Which kind of node a `new Text()` / `new Comment()` / `new DocumentFragment()`
/// makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DomNodeKind {
    Text,
    Comment,
    Fragment,
}

/// HTML element interfaces and the tag names they cover (HTML Standard §3.2.8,
/// the elements that have an interface of their own).
pub(crate) const HTML_ELEMENT_INTERFACES: &[(&str, &[&str])] = &[
    ("HTMLAnchorElement", &["a"]),
    ("HTMLAreaElement", &["area"]),
    ("HTMLAudioElement", &["audio"]),
    ("HTMLBRElement", &["br"]),
    ("HTMLBaseElement", &["base"]),
    ("HTMLBodyElement", &["body"]),
    ("HTMLButtonElement", &["button"]),
    ("HTMLCanvasElement", &["canvas"]),
    ("HTMLDListElement", &["dl"]),
    ("HTMLDataElement", &["data"]),
    ("HTMLDataListElement", &["datalist"]),
    ("HTMLDetailsElement", &["details"]),
    ("HTMLDialogElement", &["dialog"]),
    ("HTMLDivElement", &["div"]),
    ("HTMLEmbedElement", &["embed"]),
    ("HTMLFieldSetElement", &["fieldset"]),
    ("HTMLFormElement", &["form"]),
    ("HTMLHRElement", &["hr"]),
    ("HTMLHeadElement", &["head"]),
    ("HTMLHeadingElement", &["h1", "h2", "h3", "h4", "h5", "h6"]),
    ("HTMLHtmlElement", &["html"]),
    ("HTMLIFrameElement", &["iframe"]),
    ("HTMLImageElement", &["img"]),
    ("HTMLInputElement", &["input"]),
    ("HTMLLIElement", &["li"]),
    ("HTMLLabelElement", &["label"]),
    ("HTMLLegendElement", &["legend"]),
    ("HTMLLinkElement", &["link"]),
    ("HTMLMapElement", &["map"]),
    ("HTMLMediaElement", &[]),
    ("HTMLMetaElement", &["meta"]),
    ("HTMLMeterElement", &["meter"]),
    ("HTMLModElement", &["ins", "del"]),
    ("HTMLOListElement", &["ol"]),
    ("HTMLObjectElement", &["object"]),
    ("HTMLOptGroupElement", &["optgroup"]),
    ("HTMLOptionElement", &["option"]),
    ("HTMLOutputElement", &["output"]),
    ("HTMLParagraphElement", &["p"]),
    ("HTMLParamElement", &["param"]),
    ("HTMLPictureElement", &["picture"]),
    ("HTMLPreElement", &["pre"]),
    ("HTMLProgressElement", &["progress"]),
    ("HTMLQuoteElement", &["blockquote", "q"]),
    ("HTMLScriptElement", &["script"]),
    ("HTMLSelectElement", &["select"]),
    ("HTMLSlotElement", &["slot"]),
    ("HTMLSourceElement", &["source"]),
    ("HTMLSpanElement", &["span"]),
    ("HTMLStyleElement", &["style"]),
    ("HTMLTableCaptionElement", &["caption"]),
    ("HTMLTableCellElement", &["td", "th"]),
    ("HTMLTableColElement", &["col", "colgroup"]),
    ("HTMLTableElement", &["table"]),
    ("HTMLTableRowElement", &["tr"]),
    ("HTMLTableSectionElement", &["thead", "tbody", "tfoot"]),
    ("HTMLTemplateElement", &["template"]),
    ("HTMLTextAreaElement", &["textarea"]),
    ("HTMLTimeElement", &["time"]),
    ("HTMLTitleElement", &["title"]),
    ("HTMLTrackElement", &["track"]),
    ("HTMLUListElement", &["ul"]),
    ("HTMLVideoElement", &["video"]),
];

/// The `Math` functions that are a single pure `f64` operation, so one native
/// variant covers them all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MathOp {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Asinh,
    Acosh,
    Atanh,
    Log,
    Log2,
    Log10,
    Log1p,
    Exp,
    Expm1,
    Sign,
    Trunc,
    Cbrt,
    Fround,
    Clz32,
    Imul,
    Atan2,
    Hypot,
}

/// `Number.isInteger` and its siblings (ECMA-262 §21.1.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NumberOp {
    IsInteger,
    IsFinite,
    IsNaN,
    IsSafeInteger,
}

/// The accessors ECMA-262 22.2.6 defines on %RegExp.prototype%.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegExpAccessor {
    Source,
    Flags,
    Global,
    IgnoreCase,
    Multiline,
    DotAll,
    Sticky,
    Unicode,
    UnicodeSets,
    HasIndices,
}

/// The Annex B.2.2 HTML methods as `(method, tag, attribute)`, where an empty
/// attribute means the method takes no argument. `NativeFunction::StrHtml`
/// indexes this table, so a method's native and its name share one row.
pub(crate) const HTML_METHODS: [(&str, &str, &str); 13] = [
    ("anchor", "a", "name"),
    ("big", "big", ""),
    ("blink", "blink", ""),
    ("bold", "b", ""),
    ("fixed", "tt", ""),
    ("fontcolor", "font", "color"),
    ("fontsize", "font", "size"),
    ("italics", "i", ""),
    ("link", "a", "href"),
    ("small", "small", ""),
    ("strike", "strike", ""),
    ("sub", "sub", ""),
    ("sup", "sup", ""),
];

impl NativeFunction {
    /// Whether the built-in begins with `RequireObjectCoercible(this)` or
    /// `ToObject(this)`, so a `null` or `undefined` receiver is a `TypeError`.
    /// An ordinary sloppy call would substitute the global object instead,
    /// which is wrong for a built-in. The call path checks this before it
    /// substitutes anything.
    pub(crate) const fn requires_coercible_this(self) -> bool {
        matches!(
            self,
            // String.prototype (ECMA-262 22.1.3), and Symbol.iterator on it.
            Self::StrCharAt
                | Self::StrCharCodeAt
                | Self::StrCodePointAt
                | Self::StrAt
                | Self::StrIndexOf
                | Self::StrLastIndexOf
                | Self::StrIncludes
                | Self::StrStartsWith
                | Self::StrEndsWith
                | Self::StrSlice
                | Self::StrSubstring
                | Self::StringSubstr
                | Self::StrPadStart
                | Self::StrPadEnd
                | Self::StrTrim
                | Self::StrTrimStart
                | Self::StrTrimEnd
                | Self::StrRepeat
                | Self::StrLocaleCompare
                | Self::StrReplace
                | Self::StrReplaceAll
                | Self::StrSplit
                | Self::StrMatch
                | Self::StrMatchAll
                | Self::StrSearch
                | Self::StrConcat
                | Self::StrToLowerCase
                | Self::StrToUpperCase
                | Self::StrIsWellFormed
                | Self::StrToWellFormed
                | Self::StrHtml(_)
                | Self::StrIterator
                // Array.prototype (ECMA-262 23.1.3): every method begins with ToObject.
                | Self::ArrayJoin
                | Self::ArrayIndexOf
                | Self::ArraySlice
                | Self::ArrayValues
                | Self::ArrayKeys
                | Self::ArrayEntries
                | Self::ArraySplice
                | Self::ArrayReverse
                | Self::ArraySort
                | Self::ArrayConcat
                | Self::ArrayShift
                | Self::ArrayUnshift
                | Self::ArrayForEach
                | Self::ArrayMap
                | Self::ArrayFilter
                | Self::ArraySome
                | Self::ArrayFind
                | Self::ArrayFindIndex
                | Self::ArrayFindLast
                | Self::ArrayFindLastIndex
                | Self::ArrayAt
                | Self::ArrayFlat
                | Self::ArrayReduceRight
                | Self::ArrayEvery
                | Self::ArrayIncludes
                | Self::ArrayReduce
                | Self::ArrayPrototypeToString
                | Self::ArrayPush
                | Self::ArrayPop
                // Object.prototype methods that begin with ToObject(this).
                | Self::ObjectPrototypeHasOwnProperty
                | Self::ObjectPrototypePropertyIsEnumerable
                // Date.prototype.toJSON begins with ToObject(this) (ECMA-262 21.4.4.37).
                | Self::DateToJSON
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeFunction {
    MathOp(MathOp),
    NumberOp(NumberOp),
    GetElementById,
    QuerySelector,
    QuerySelectorAll,
    GetElementsByTagName,
    GetElementsByClassName,
    CloneNode,
    NamedMapItem,
    NamedMapGetNamedItem,
    AttrGetName,
    AttrGetValue,
    CreateTextNode,
    CreateComment,
    CreateDocumentFragment,
    CreateEvent,
    GetComputedStyle,
    GlobalParseInt,
    GlobalParseFloat,
    GlobalIsNaN,
    GlobalIsFinite,
    GlobalEncodeURI,
    GlobalEncodeURIComponent,
    GlobalDecodeURI,
    GlobalDecodeURIComponent,
    GlobalEscape,
    GlobalUnescape,
    GlobalAtob,
    GlobalBtoa,
    GlobalRandomBytes,
    GlobalEvalStub,
    GlobalImport,
    GlobalNoop,
    HistoryPushState,
    HistoryReplaceState,
    HistoryBack,
    HistoryForward,
    HistoryGo,
    CssSupports,
    UrlSearchParamsGet,
    UrlSearchParamsHas,
    UrlSearchParamsSet,
    UrlSearchParamsAppend,
    UrlSearchParamsToString,
    UrlSearchParamsForEach,
    UrlToString,
    SymbolToString,
    SymbolValueOf,
    BigIntAsIntN,
    BigIntAsUintN,
    BigIntToString,
    BigIntValueOf,
    NumToFixed,
    NumToPrecision,
    NumToString,
    NumValueOf,
    BoolToString,
    BoolValueOf,
    WindowAddEventListener,
    WindowRemoveEventListener,
    CompareDocumentPosition,
    CreateElement,
    SetAttribute,
    GetAttribute,
    HasAttribute,
    RemoveAttribute,
    AppendChild,
    RemoveChild,
    InsertBefore,
    RemoveNode,
    Contains,
    Matches,
    Click,
    AddEventListener,
    RemoveEventListener,
    DispatchEvent,
    EventPreventDefault,
    EventStopPropagation,
    EventStopImmediatePropagation,
    EventComposedPath,
    ClassListAdd,
    ClassListRemove,
    ClassListToggle,
    ClassListContains,
    ClassListItem,
    ClassListToString,
    LocationToString,
    LocationAssign,
    LocationReplace,
    RegExpExec,
    RegExpTest,
    RegExpToString,
    RegExpAccessor(RegExpAccessor),
    /// `RegExp.prototype[@@match]`, `[@@matchAll]`, `[@@replace]`, `[@@search]`
    /// and `[@@split]` (ECMA-262 22.2.6.8-12).
    RegExpSymbolMatch,
    RegExpSymbolMatchAll,
    RegExpSymbolReplace,
    RegExpSymbolSearch,
    RegExpSymbolSplit,
    /// `%RegExpStringIteratorPrototype%.next` (ECMA-262 22.2.9.2.1).
    RegExpStringIteratorNext,
    /// `RegExp.escape` (the RegExp.escape proposal).
    RegExpEscape,
    /// The `get [Symbol.species]` getter on the `RegExp` constructor.
    RegExpSpecies,
    StrCharAt,
    StrCharCodeAt,
    /// ECMA-262 22.1.3.12. The sibling of `charCodeAt` that answers the scalar
    /// value of the code point *starting* at a position, so a surrogate pair
    /// reports the astral scalar rather than a half.
    StrCodePointAt,
    /// ECMA-262 22.1.3.3, the shared `at` used by every indexed collection.
    StrAt,
    StrPadStart,
    StrPadEnd,
    StrTrimStart,
    StrTrimEnd,
    StrRepeat,
    StrLocaleCompare,
    StrReplaceAll,
    /// ECMA-262 22.1.3.10 `String.prototype.isWellFormed` (ES2024).
    StrIsWellFormed,
    /// ECMA-262 22.1.3.32 `String.prototype.toWellFormed` (ES2024).
    StrToWellFormed,
    /// Annex B.2.2 `CreateHTML`, for the method at `index` in [`HTML_METHODS`].
    StrHtml(usize),
    StringFromCharCode,
    StringFromCodePoint,
    StringRaw,
    StrIndexOf,
    StrLastIndexOf,
    StrIncludes,
    StrStartsWith,
    StrEndsWith,
    StrSlice,
    StrSubstring,
    StrToLowerCase,
    StrToUpperCase,
    StrTrim,
    StrSplit,
    StrReplace,
    StrMatch,
    StrMatchAll,
    StrSearch,
    StrConcat,
    StrToString,
    StrForEach,
    StrPush,
    StrIterator,
    ConsoleDebug,
    ConsoleError,
    ConsoleInfo,
    ConsoleLog,
    ConsoleWarn,
    SetTimeout,
    SetInterval,
    ClearTimeout,
    ClearInterval,
    RequestAnimationFrame,
    CancelAnimationFrame,
    GetBoundingClientRect,
    IntersectionObserve,
    IntersectionUnobserve,
    IntersectionDisconnect,
    IntersectionTakeRecords,
    StyleGetProperty,
    StyleSetProperty,
    StyleRemoveProperty,
    StyleItem,
    QueueMicrotask,
    PromiseResolve,
    PromiseReject,
    PromiseThen,
    PromiseFinally,
    PromiseFinallyPass,
    PromiseFinallyReject,
    PromiseCatch,
    PromiseSpecies,
    PromiseWithResolvers,
    PromiseTry,
    PromiseValueThunk,
    PromiseThrower,
    PromiseResolveThenableJob,
    PromiseCapabilityExecutor,
    ArrayIsArray,
    ArrayFrom,
    ArrayPush,
    ArrayPop,
    ArrayJoin,
    ArrayIndexOf,
    ArraySlice,
    /// `Array.prototype.values` / `keys` / `entries`.
    ArrayValues,
    ArrayKeys,
    ArrayEntries,
    ArraySplice,
    ArrayReverse,
    ArraySort,
    ArrayConcat,
    ArrayShift,
    ArrayUnshift,
    ArrayForEach,
    ArrayMap,
    ArrayFilter,
    ArraySome,
    ArrayFind,
    ArrayFindIndex,
    ArrayFindLast,
    ArrayFindLastIndex,
    ArrayAt,
    ArrayFlat,
    ArrayReduceRight,
    ArrayEvery,
    ArrayIncludes,
    ArrayReduce,
    ArrayLastIndexOf,
    ArrayFlatMap,
    ArrayToLocaleString,
    ArrayOf,
    ArraySpecies,
    ArrayToSorted,
    ArrayToReversed,
    ArrayToSpliced,
    ArrayWith,
    ArrayFill,
    ArrayCopyWithin,
    ArrayFromAsync,
    FunctionPrototype,
    FunctionToString,
    FunctionCall,
    FunctionBind,
    FunctionApply,
    FunctionHasInstance,
    DateSetTime,
    DateSetDate,
    DateSetFullYear,
    DateSetHours,
    DateSetMilliseconds,
    DateSetMinutes,
    DateSetMonth,
    DateSetSeconds,
    DateSetUTCDate,
    DateSetUTCFullYear,
    DateSetUTCHours,
    DateSetUTCMilliseconds,
    DateSetUTCMinutes,
    DateSetUTCMonth,
    DateSetUTCSeconds,
    DateSetYear,
    DateGetYear,
    DateToTimeString,
    DateToLocaleString,
    DateToLocaleDateString,
    DateToLocaleTimeString,
    DateSymbolToPrimitive,
    DateGetFullYear,
    DateGetMonth,
    DateGetDate,
    DateGetDay,
    DateGetHours,
    DateGetMinutes,
    DateGetSeconds,
    DateGetMilliseconds,
    DateGetTimezoneOffset,
    DateGetUTCFullYear,
    DateGetUTCMonth,
    DateGetUTCDate,
    DateGetUTCDay,
    DateGetUTCHours,
    DateGetUTCMinutes,
    DateGetUTCSeconds,
    DateGetUTCMilliseconds,
    DateToGMTString,
    DateToDateString,
    DateToISOString,
    DateToJSON,
    DateParse,
    DateUTC,
    StringSubstr,
    MathAbs,
    MathCeil,
    MathFloor,
    MathMax,
    MathMin,
    MathPow,
    MathRandom,
    MathRound,
    MathSqrt,
    ObjectAssign,
    ObjectKeys,
    ObjectValues,
    ObjectEntries,
    ObjectCreate,
    ObjectDefineProperty,
    ObjectDefineProperties,
    ObjectGetOwnPropertyDescriptor,
    ObjectGetOwnPropertyDescriptors,
    ObjectGetOwnPropertyNames,
    ObjectGetOwnPropertySymbols,
    ObjectGetPrototypeOf,
    ObjectSetPrototypeOf,
    ObjectHasOwn,
    ObjectPrototypeHasOwnProperty,
    ObjectPrototypeIsPrototypeOf,
    ObjectPrototypePropertyIsEnumerable,
    ObjectPrototypeToString,
    ObjectPrototypeToLocaleString,
    ObjectDefineGetter,
    SymbolDescription,
    SymbolFor,
    SymbolKeyFor,
    ObjectPreventExtensions,
    ObjectProtoGetter,
    ObjectProtoSetter,
    StorageGetItem,
    StorageSetItem,
    StorageRemoveItem,
    StorageClear,
    StorageKey,
    ObjectSeal,
    ObjectFreeze,
    ObjectIsExtensible,
    ObjectIsSealed,
    ObjectIsFrozen,
    ObjectDefineSetter,
    ObjectLookupGetter,
    ObjectLookupSetter,
    ObjectPrototypeValueOf,
    DateNow,
    DateGetValue,
    DateValueOf,
    DateToString,
    ErrorPrototypeToString,
    /// The `name`, `message` and `code` accessors of `DOMException.prototype`.
    /// One native per member because `WebIDL` attributes are separate accessors,
    /// and the three read different internal slots.
    DomExceptionNameGetter,
    DomExceptionMessageGetter,
    DomExceptionCodeGetter,
    /// `window.matchMedia(query)`, the `MediaQueryList` `media`/`matches`
    /// accessors, `addEventListener` / `removeEventListener` and the deprecated
    /// `addListener` / `removeListener`. One native per member because the
    /// attributes are separate accessors and the last two are legacy aliases
    /// with their own arities and their own "return the listener" contract.
    WindowMatchMedia,
    MediaQueryListMediaGetter,
    MediaQueryListMatchesGetter,
    MediaQueryListAddEventListener,
    MediaQueryListRemoveEventListener,
    MediaQueryListAddListener,
    MediaQueryListRemoveListener,
    JsonParse,
    JsonStringify,
    PerformanceNow,
    PerformanceGetEntries,
    PerformanceGetEntriesByType,
    CollectionGet,
    CollectionSet,
    CollectionAdd,
    CollectionHas,
    CollectionDelete,
    CollectionClear,
    CollectionForEach,
    CollectionKeys,
    CollectionValues,
    CollectionEntries,
    CollectionUnion,
    CollectionIntersection,
    CollectionDifference,
    CollectionSymmetricDifference,
    CollectionIsSubsetOf,
    CollectionIsSupersetOf,
    CollectionIsDisjointFrom,
    CollectionIteratorNext,
    ArrayIteratorNext,
    TypedArraySet,
    TypedArraySubarray,
    TypedArraySlice,
    TypedArrayFill,
    TypedArrayIndexOf,
    TypedArrayJoin,
    TypedArrayFrom,
    TypedArrayIncludes,
    TypedArrayForEach,
    TypedArrayMap,
    TypedArrayFilter,
    /// `%TypedArray%.prototype.values`, which the specification also installs
    /// as `%TypedArray%.prototype[@@iterator]`.
    TypedArrayValues,
    /// `%TypedArray%` itself, the abstract constructor that throws when it is
    /// called or constructed.
    TypedArrayIntrinsic,
    TypedArrayOf,
    TypedArrayKeys,
    TypedArrayEntries,
    TypedArrayAt,
    TypedArrayCopyWithin,
    TypedArrayEvery,
    TypedArraySome,
    TypedArrayFind,
    TypedArrayFindIndex,
    TypedArrayFindLast,
    TypedArrayFindLastIndex,
    TypedArrayLastIndexOf,
    TypedArrayReduce,
    TypedArrayReduceRight,
    TypedArrayReverse,
    TypedArraySort,
    TypedArrayToReversed,
    TypedArrayToSorted,
    TypedArrayWith,
    /// The `length`, `byteLength`, `byteOffset` and `@@toStringTag` accessors of
    /// `%TypedArray%.prototype`.
    TypedArrayLengthGetter,
    TypedArrayByteLengthGetter,
    TypedArrayByteOffsetGetter,
    TypedArrayToStringTagGetter,
    PromiseAll,
    PromiseAllSettled,
    PromiseAllKeyed,
    PromiseAllSettledKeyed,
    PromiseAny,
    PromiseRace,
    /// One element's settlement function of a combinator. Bound with the store,
    /// the element's index, its pair and its role.
    PromiseCombinatorElement,
    /// A method of `DisposableStack` (`DisposeStackKind::Sync`) or
    /// `AsyncDisposableStack` (`DisposeStackKind::Async`).
    DisposeStack(DisposeStackOp, DisposeStackKind),
    MutationObserve,
    MutationDisconnect,
    MutationTakeRecords,
    ArrayPrototypeToString,
    /// Iterator helpers (`%IteratorPrototype%` methods).
    IteratorConstructor,
    IteratorFrom,
    IteratorPrototypeIterator,
    GeneratorNext,
    GeneratorReturn,
    GeneratorThrow,
    AsyncGeneratorNext,
    AsyncGeneratorReturn,
    AsyncGeneratorThrow,
    AsyncFromSyncNext,
    AsyncFromSyncReturn,
    AsyncFromSyncThrow,
    IteratorHelperNext,
    IteratorHelperReturn,
    IteratorMap,
    IteratorFilter,
    IteratorTake,
    IteratorDrop,
    IteratorFlatMap,
    IteratorReduce,
    IteratorToArray,
    IteratorForEach,
    IteratorSome,
    IteratorEvery,
    IteratorFind,
    IteratorConcat,
    IteratorChunks,
    IteratorWindows,
    IteratorIncludes,
    IteratorJoin,
    IteratorWrapNext,
    IteratorWrapReturn,
    IteratorZip,
    IteratorZipKeyed,
    GlobalFetch,
    ReflectGet,
    ReflectSet,
    ReflectHas,
    ReflectDeleteProperty,
    ReflectOwnKeys,
    ReflectGetOwnPropertyDescriptor,
    ReflectDefineProperty,
    ReflectConstruct,
    ReflectApply,
    ReflectGetPrototypeOf,
    ReflectSetPrototypeOf,
    ReflectIsExtensible,
    ReflectPreventExtensions,
    ProxyRevocable,
    ProxyRevoke,
    ResponseText,
    ResponseJson,
    ResponseHeadersGet,
    BlobText,
    BlobArrayBuffer,
    BlobSlice,
    UrlCreateObjectUrl,
    UrlRevokeObjectUrl,
    XhrOpen,
    XhrSetRequestHeader,
    XhrSend,
    XhrGetResponseHeader,
    XhrGetAllResponseHeaders,
    XhrAddEventListener,
    XhrRemoveEventListener,
    AbortControllerAbort,
    FormDataAppend,
    FormDataGet,
    FormDataSet,
    FormDataHas,
    FormDataDelete,
    FormDataEntries,
    TextEncoderEncode,
    TextEncoderEncodeInto,
    TextDecoderDecode,
    DataViewGetInt8,
    DataViewGetUint8,
    DataViewGetInt16,
    DataViewGetUint16,
    DataViewGetInt32,
    DataViewGetUint32,
    DataViewGetFloat32,
    DataViewGetFloat64,
    DataViewGetBigInt64,
    DataViewGetBigUint64,
    DataViewSetInt8,
    DataViewSetUint8,
    DataViewSetInt16,
    DataViewSetUint16,
    DataViewSetInt32,
    DataViewSetUint32,
    DataViewSetFloat32,
    DataViewSetFloat64,
    DataViewSetBigInt64,
    DataViewSetBigUint64,
    DataViewGetFloat16,
    DataViewSetFloat16,
    /// `DataView.prototype.buffer`, `byteLength` and `byteOffset`: accessors on
    /// the prototype, not own data properties of each view.
    DataViewBufferGetter,
    DataViewByteLengthGetter,
    DataViewByteOffsetGetter,
    ArrayBufferSlice,
    /// `ArrayBuffer.prototype.byteLength`, an accessor on the prototype.
    ArrayBufferByteLengthGetter,
    /// `ArrayBuffer.isView(arg)` (ECMA-262 25.1.5.1).
    ArrayBufferIsView,
    /// `get ArrayBuffer[@@species]` and `get %TypedArray%[@@species]`: `this`.
    ArrayBufferSpecies,
    TypedArraySpecies,
    /// The resizable-buffer surface of `ArrayBuffer.prototype` (ECMA-262 25.1.6):
    /// the `resizable`, `maxByteLength` and `detached` accessors, `resize`, and
    /// the two transfer methods.
    ArrayBufferResizableGetter,
    ArrayBufferMaxByteLengthGetter,
    ArrayBufferDetachedGetter,
    ArrayBufferResize,
    ArrayBufferTransfer,
    ArrayBufferTransferToFixedLength,
    /// The `SharedArrayBuffer.prototype` members (ECMA-262 25.2.5): the
    /// `byteLength`, `growable` and `maxByteLength` accessors, `grow` and `slice`.
    SharedArrayBufferByteLengthGetter,
    SharedArrayBufferGrowableGetter,
    SharedArrayBufferMaxByteLengthGetter,
    SharedArrayBufferGrow,
    SharedArrayBufferSlice,
    GlobalStructuredClone,
    VideoPlay,
    VideoPause,
    VideoLoad,
    VideoCanPlayType,
    /// `%TypedArray%.prototype.buffer`: the `ArrayBuffer` the view is over.
    TypedArrayBufferGetter,
    /// A function an embedder defined with `JsRuntime::define_host_function`: the
    /// index of its entry in the runtime's host-function table.
    Host(usize),
}

/// One integer or float element type of the ECMAScript typed-array family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypedArrayKind {
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float32,
    Float64,
    /// `BigInt64Array` (ECMA-262 23.2.7): eight bytes read as a signed `BigInt`.
    BigInt64,
    /// `BigUint64Array`: eight bytes read as an unsigned `BigInt`.
    BigUint64,
}

impl TypedArrayKind {
    pub(crate) const ALL: [Self; 11] = [
        Self::Int8,
        Self::Uint8,
        Self::Uint8Clamped,
        Self::Int16,
        Self::Uint16,
        Self::Int32,
        Self::Uint32,
        Self::Float32,
        Self::Float64,
        Self::BigInt64,
        Self::BigUint64,
    ];

    /// Whether elements hold a `BigInt` rather than a Number.
    pub(crate) const fn is_bigint(self) -> bool {
        matches!(self, Self::BigInt64 | Self::BigUint64)
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Int8 => "Int8Array",
            Self::Uint8 => "Uint8Array",
            Self::Uint8Clamped => "Uint8ClampedArray",
            Self::Int16 => "Int16Array",
            Self::Uint16 => "Uint16Array",
            Self::Int32 => "Int32Array",
            Self::Uint32 => "Uint32Array",
            Self::Float32 => "Float32Array",
            Self::Float64 => "Float64Array",
            Self::BigInt64 => "BigInt64Array",
            Self::BigUint64 => "BigUint64Array",
        }
    }

    pub(crate) const fn element_size(self) -> usize {
        match self {
            Self::Int8 | Self::Uint8 | Self::Uint8Clamped => 1,
            Self::Int16 | Self::Uint16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float32 => 4,
            Self::Float64 | Self::BigInt64 | Self::BigUint64 => 8,
        }
    }

    /// Decode one `BigInt` element from its little-endian bytes (ECMA-262
    /// 10.4.5.12 `RawBytesToNumeric` for the `BigInt` element types).
    pub(crate) fn load_bigint(self, bytes: &[u8]) -> JsBigInt {
        let mut raw = [0_u8; 8];
        raw.copy_from_slice(&bytes[..8]);
        match self {
            Self::BigInt64 => JsBigInt::from_i64(i64::from_le_bytes(raw)),
            _ => JsBigInt::from_u64(u64::from_le_bytes(raw)),
        }
    }

    /// Store a `BigInt` element as its value modulo 2^64 (ECMA-262 7.1.15
    /// `ToBigInt64` and 7.1.16 `ToBigUint64`), little-endian.
    pub(crate) fn store_bigint(value: &JsBigInt, bytes: &mut [u8]) {
        let modulo = value
            .as_uint_n(64)
            .and_then(|wrapped| wrapped.magnitude_u64())
            .unwrap_or(0);
        bytes.copy_from_slice(&modulo.to_le_bytes());
    }

    /// Convert a JavaScript Number into one element of this array kind,
    /// applying the integer-indexed wrapping (`Int8` through `Uint32`),
    /// clamping (`Uint8Clamped`), or float rounding (`Float32`) rules of
    /// the ECMA-262 `IntegerIndexedElementSet` operation.
    fn encode(self, value: f64) -> f64 {
        match self {
            // BigInt elements convert through `ToBigInt`, never a Number.
            Self::Float64 | Self::BigInt64 | Self::BigUint64 => value,
            Self::Float32 => {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "Float32Array elements round to IEEE binary32"
                )]
                {
                    f64::from(value as f32)
                }
            }
            Self::Uint8Clamped => Self::clamp_u8(value),
            Self::Uint8 => Self::wrap_integer(value, 8, false),
            Self::Int8 => Self::wrap_integer(value, 8, true),
            Self::Uint16 => Self::wrap_integer(value, 16, false),
            Self::Int16 => Self::wrap_integer(value, 16, true),
            Self::Uint32 => Self::wrap_integer(value, 32, false),
            Self::Int32 => Self::wrap_integer(value, 32, true),
        }
    }

    /// Decode one element from the little-endian bytes of this kind (ECMA-262
    /// 10.4.5.12 `RawBytesToNumeric`). `bytes` holds at least `element_size` bytes.
    #[allow(
        clippy::cast_precision_loss,
        reason = "BigInt elements have an approximate Number value for generic numeric code only"
    )]
    pub(crate) fn load(self, bytes: &[u8]) -> f64 {
        match self {
            Self::Int8 => f64::from(i8::from_le_bytes(le_bytes(bytes))),
            Self::Uint8 | Self::Uint8Clamped => f64::from(u8::from_le_bytes(le_bytes(bytes))),
            Self::Int16 => f64::from(i16::from_le_bytes(le_bytes(bytes))),
            Self::Uint16 => f64::from(u16::from_le_bytes(le_bytes(bytes))),
            Self::Int32 => f64::from(i32::from_le_bytes(le_bytes(bytes))),
            Self::Uint32 => f64::from(u32::from_le_bytes(le_bytes(bytes))),
            Self::Float32 => f64::from(f32::from_le_bytes(le_bytes(bytes))),
            Self::Float64 => f64::from_le_bytes(le_bytes(bytes)),
            // A BigInt element has no Number value; this approximation serves
            // only generic numeric code, which reads BigInt kinds through
            // `element_value` instead.
            Self::BigInt64 => i64::from_le_bytes(le_bytes(bytes)) as f64,
            Self::BigUint64 => u64::from_le_bytes(le_bytes(bytes)) as f64,
        }
    }

    /// Encode a JavaScript Number as one element of this kind, as little-endian
    /// bytes (ECMA-262 10.4.5.11 `NumericToRawBytes`). `bytes` is exactly
    /// `element_size` long.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the converted value is already within this kind's range"
    )]
    pub(crate) fn store(self, value: f64, bytes: &mut [u8]) {
        let value = self.encode(value);
        match self {
            Self::Int8 | Self::Uint8 | Self::Uint8Clamped => {
                bytes.copy_from_slice(&(value as i64 as u8).to_le_bytes());
            }
            Self::Int16 | Self::Uint16 => {
                bytes.copy_from_slice(&(value as i64 as u16).to_le_bytes());
            }
            Self::Int32 | Self::Uint32 => {
                bytes.copy_from_slice(&(value as i64 as u32).to_le_bytes());
            }
            Self::Float32 => bytes.copy_from_slice(&(value as f32).to_le_bytes()),
            Self::Float64 => bytes.copy_from_slice(&value.to_le_bytes()),
            // Numeric stores into a BigInt kind are approximations; BigInt
            // elements are written through `set_bigint_element`.
            Self::BigInt64 | Self::BigUint64 => {
                bytes.copy_from_slice(&(value as i64).to_le_bytes());
            }
        }
    }

    fn wrap_integer(value: f64, bits: u32, signed: bool) -> f64 {
        if !value.is_finite() {
            return 0.0;
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "integer-indexed stores truncate toward zero first"
        )]
        let truncated = value.trunc();
        #[allow(
            clippy::cast_precision_loss,
            reason = "element-type ranges stay far below any precision boundary"
        )]
        let modulus = (1_u64 << bits) as f64;
        let wrapped = truncated.rem_euclid(modulus);
        if signed && wrapped >= modulus / 2.0 {
            wrapped - modulus
        } else {
            wrapped
        }
    }

    fn clamp_u8(value: f64) -> f64 {
        if value.is_nan() {
            return 0.0;
        }
        if value <= 0.0 {
            return 0.0;
        }
        if value >= 255.0 {
            return 255.0;
        }
        let floor = value.floor();
        let fraction = value - floor;
        let half_is_even = (floor / 2.0).fract() == 0.0;
        match fraction.partial_cmp(&0.5) {
            Some(std::cmp::Ordering::Less) => floor,
            Some(std::cmp::Ordering::Equal) if half_is_even => floor,
            _ => floor + 1.0,
        }
    }
}

/// The shared backing store of one `ArrayBuffer` (ECMA-262 25.1.3). Typed arrays
/// and `DataView`s hold a clone of the `Rc`, so every view of a buffer reads and
/// writes the same bytes, and a detach is seen through all of them.
#[derive(Clone, Debug, Default)]
pub(crate) struct TypedBuffer(pub std::rc::Rc<std::cell::RefCell<BufferStore>>);

#[derive(Debug, Default)]
pub(crate) struct BufferStore {
    /// The bytes. Multi-byte elements are little-endian (ECMA-262 10.4.5.12).
    bytes: Vec<u8>,
    /// Set by `DetachArrayBuffer` (ECMA-262 25.1.3.5). A detached buffer has no
    /// bytes, and access through any view throws a `TypeError`.
    detached: bool,
    /// The `ArrayBuffer` object this store belongs to, once one exists. A view's
    /// `buffer` getter returns it, so the collector keeps it alive.
    object: Option<ObjectId>,
    /// `[[ArrayBufferMaxByteLength]]` (ECMA-262 25.1.3.1). `Some` makes the
    /// buffer resizable (or, shared, growable); `None` is a fixed-length buffer.
    max_byte_length: Option<usize>,
    /// Whether this is a `SharedArrayBuffer` (ECMA-262 25.2). It never detaches,
    /// and its `ArrayBuffer` and `SharedArrayBuffer` brands are kept apart.
    shared: bool,
}

impl PartialEq for TypedBuffer {
    fn eq(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.0, &other.0)
    }
}

/// The first `N` bytes of `bytes` as an array, for decoding one element.
fn le_bytes<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut array = [0_u8; N];
    array.copy_from_slice(&bytes[..N]);
    array
}

impl TypedBuffer {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(BufferStore {
            bytes,
            ..BufferStore::default()
        })))
    }

    /// A resizable buffer of `bytes` whose `[[ArrayBufferMaxByteLength]]` is
    /// `max_byte_length`.
    pub(crate) fn new_resizable(bytes: Vec<u8>, max_byte_length: usize) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(BufferStore {
            bytes,
            max_byte_length: Some(max_byte_length),
            ..BufferStore::default()
        })))
    }

    /// A `SharedArrayBuffer` of `bytes`, growable up to `max_byte_length` when
    /// that is given.
    pub(crate) fn new_shared(bytes: Vec<u8>, max_byte_length: Option<usize>) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(BufferStore {
            bytes,
            max_byte_length,
            shared: true,
            ..BufferStore::default()
        })))
    }

    pub(crate) fn is_shared(&self) -> bool {
        self.0.borrow().shared
    }

    pub(crate) fn byte_length(&self) -> usize {
        self.0.borrow().bytes.len()
    }

    /// `[[ArrayBufferMaxByteLength]]`, or `None` for a fixed-length buffer.
    pub(crate) fn max_byte_length(&self) -> Option<usize> {
        self.0.borrow().max_byte_length
    }

    /// Set the byte length, zero-filling any growth. The caller has checked the
    /// new length against the maximum and the engine bound.
    pub(crate) fn resize(&self, new_length: usize) {
        self.0.borrow_mut().bytes.resize(new_length, 0);
    }

    pub(crate) fn is_detached(&self) -> bool {
        self.0.borrow().detached
    }

    /// The element count of a view over this store, in `unit`-byte units: `start`
    /// is the view's offset in those units and `length` its count, or `None` for
    /// a length-tracking view. Returns `None` when the view is out of bounds or
    /// the buffer is detached (ECMA-262 10.4.5.12 `IsTypedArrayOutOfBounds` and
    /// `TypedArrayLength`, and 25.2.4 `IsViewOutOfBounds`).
    pub(crate) fn view_length(
        &self,
        unit: usize,
        start: usize,
        length: Option<usize>,
    ) -> Option<usize> {
        let store = self.0.borrow();
        if store.detached {
            return None;
        }
        let total = store.bytes.len();
        let begin = start * unit;
        match length {
            Some(length) => (begin + length * unit <= total).then_some(length),
            None => (begin <= total).then(|| (total - begin) / unit),
        }
    }

    /// The bytes an in-bounds view covers, or a `TypeError` when it is out of
    /// bounds (the `ValidateTypedArray` and `GetViewValue` rule).
    pub(crate) fn view_bytes(
        &self,
        unit: usize,
        start: usize,
        length: Option<usize>,
    ) -> Result<Vec<u8>, JsError> {
        let length = self.view_length(unit, start, length).ok_or_else(|| {
            JsError::type_error("typed array or DataView is out of bounds of its ArrayBuffer")
        })?;
        self.read_bytes(start * unit, length * unit)
    }

    /// `TypeError` unless the buffer is attached: every view access starts here.
    pub(crate) fn ensure_attached(&self) -> Result<(), JsError> {
        if self.is_detached() {
            return Err(JsError::type_error(
                "operation on a typed array or DataView whose ArrayBuffer is detached",
            ));
        }
        Ok(())
    }

    /// `DetachArrayBuffer` (ECMA-262 25.1.3.5): releases the bytes and marks
    /// every view of this store detached.
    pub(crate) fn detach(&self) {
        let mut store = self.0.borrow_mut();
        store.bytes = Vec::new();
        store.detached = true;
    }

    /// The `ArrayBuffer` object this store belongs to, if one exists yet.
    pub(crate) fn object(&self) -> Option<ObjectId> {
        self.0.borrow().object
    }

    /// Record `object` as this store's identity, unless one is recorded already.
    pub(crate) fn identify(&self, object: ObjectId) {
        self.0.borrow_mut().object.get_or_insert(object);
    }

    /// Write `bytes` at byte offset `at`. Bytes past the end of the store are
    /// ignored, as an out-of-range typed-array store is.
    pub(crate) fn write_bytes(&self, at: usize, bytes: &[u8]) {
        let mut store = self.0.borrow_mut();
        if let Some(target) = store.bytes.get_mut(at..at + bytes.len()) {
            target.copy_from_slice(bytes);
        }
    }

    /// Copy of `length` bytes from byte offset `at`.
    pub(crate) fn read_bytes(&self, at: usize, length: usize) -> Result<Vec<u8>, JsError> {
        self.ensure_attached()?;
        self.0
            .borrow()
            .bytes
            .get(at..at + length)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| JsError::type_error("byte range is outside the ArrayBuffer"))
    }

    /// A copy of every byte of the store.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.0.borrow().bytes.clone()
    }

    /// Element `index` as the JavaScript value it reads as: a Number, or a
    /// `BigInt` for the `BigInt` kinds.
    pub(crate) fn element_value(&self, kind: TypedArrayKind, index: usize) -> Option<JsValue> {
        let size = kind.element_size();
        let store = self.0.borrow();
        store
            .bytes
            .get(index * size..(index + 1) * size)
            .map(|bytes| {
                if kind.is_bigint() {
                    JsValue::BigInt(kind.load_bigint(bytes))
                } else {
                    JsValue::Number(kind.load(bytes))
                }
            })
    }

    /// Element `first` onward, `count` of them, as the values they read as. A
    /// missing element means the buffer was detached, which is a `TypeError`.
    pub(crate) fn elements_value(
        &self,
        kind: TypedArrayKind,
        first: usize,
        count: usize,
    ) -> Result<Vec<JsValue>, JsError> {
        self.ensure_attached()?;
        (first..first + count)
            .map(|index| {
                self.element_value(kind, index)
                    .ok_or_else(|| JsError::type_error("typed array element is out of range"))
            })
            .collect()
    }

    /// Store `value`, already converted to `kind` (a `BigInt` for the `BigInt`
    /// kinds, a Number otherwise), as element `index`.
    pub(crate) fn set_converted_element(
        &self,
        kind: TypedArrayKind,
        index: usize,
        value: &JsValue,
    ) {
        match value {
            JsValue::BigInt(bigint) if kind.is_bigint() => {
                self.set_bigint_element(kind, index, bigint);
            }
            JsValue::Number(number) if !kind.is_bigint() => {
                self.set_element(kind, index, *number);
            }
            _ => {}
        }
    }

    /// Store `value` as element `index` of a `BigInt` kind, wrapping modulo 2^64.
    /// An index past the end of the store is ignored.
    pub(crate) fn set_bigint_element(&self, kind: TypedArrayKind, index: usize, value: &JsBigInt) {
        let size = kind.element_size();
        let mut store = self.0.borrow_mut();
        if let Some(bytes) = store.bytes.get_mut(index * size..(index + 1) * size) {
            TypedArrayKind::store_bigint(value, bytes);
        }
    }

    /// Store `value`, converted to `kind`, as element `index`. An index past the
    /// end of the store is ignored.
    pub(crate) fn set_element(&self, kind: TypedArrayKind, index: usize, value: f64) {
        let size = kind.element_size();
        let mut store = self.0.borrow_mut();
        if let Some(bytes) = store.bytes.get_mut(index * size..(index + 1) * size) {
            kind.store(value, bytes);
        }
    }
}

/// Which projection an array iterator yields: `keys`, `values` or `entries`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArrayView {
    Keys,
    Values,
    Entries,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CollectionKind {
    Map,
    WeakMap,
    Set,
    WeakSet,
}

impl CollectionKind {
    pub(crate) const fn is_map(self) -> bool {
        matches!(self, Self::Map | Self::WeakMap)
    }

    pub(crate) const fn is_weak(self) -> bool {
        matches!(self, Self::WeakMap | Self::WeakSet)
    }
}

/// The encodings this engine implements for `TextEncoder`/`TextDecoder`
/// (Encoding Standard). A label the Encoding Standard knows but that is not in
/// this set is a `RangeError` at construction, which is a loud failure rather
/// than a silent mis-decode; see `runtime::builtins::encoding`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextEncoding {
    Utf8,
    /// ISO-8859-1's practical superset: the `latin1`/`iso-8859-1`/`ascii`
    /// labels all name this, so `0x80` is EURO SIGN and not U+0080.
    Windows1252,
    XUserDefined,
}

impl TextEncoding {
    /// The value `TextDecoder.prototype.encoding` reports, which is the
    /// Encoding Standard's canonical name rather than the supplied label.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Windows1252 => "windows-1252",
            Self::XUserDefined => "x-user-defined",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    Error,
    EvalError,
    RangeError,
    ReferenceError,
    SyntaxError,
    TypeError,
    UriError,
}

impl ErrorKind {
    pub(crate) const ALL: [Self; 7] = [
        Self::Error,
        Self::EvalError,
        Self::RangeError,
        Self::ReferenceError,
        Self::SyntaxError,
        Self::TypeError,
        Self::UriError,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::EvalError => "EvalError",
            Self::RangeError => "RangeError",
            Self::ReferenceError => "ReferenceError",
            Self::SyntaxError => "SyntaxError",
            Self::TypeError => "TypeError",
            Self::UriError => "URIError",
        }
    }
}

/// The legacy numeric codes `WebIDL` §4.4 declares as `DOMException`'s
/// constants, and §2.8.1's names table, in one place.
///
/// The two lists are not the same, and keeping them apart is the point:
///
/// - The IDL declares 25 constants, `INDEX_SIZE_ERR = 1` through
///   `DATA_CLONE_ERR = 25`, and all 25 are exposed on both the interface object
///   and the interface prototype object (`WebIDL` §2.5.1: "the constant value can
///   be accessed in JavaScript either as `A.rambaldi` or `instanceOfA.rambaldi`").
///   Three of them - `DOMSTRING_SIZE_ERR = 2`, `NO_DATA_ALLOWED_ERR = 6` and
///   `VALIDATION_ERR = 16` - name a legacy code with **no** entry in the names
///   table, so `new DOMException("m", "NoDataAllowedError").code` is `0` while
///   `DOMException.NO_DATA_ALLOWED_ERR` is `6`.
/// - §4.4's code getter is defined purely by the names table, so eleven names
///   that have no legacy code (`EncodingError`, `NotAllowedError`, and the nine
///   IndexedDB-era ones) all report `0`, and so does any name a caller made
///   up.
///
/// One table would be a smaller thing to get wrong and a bigger thing to have
/// wrong silently, because a `code` that disagrees with the constant of the same
/// name is exactly the kind of defect no test in this crate would notice. So the
/// two are separate lists and the test asserts both the code and the constant.
const DOM_EXCEPTION_LEGACY_CODES: [(&str, u16); 25] = [
    ("INDEX_SIZE_ERR", 1),
    ("DOMSTRING_SIZE_ERR", 2),
    ("HIERARCHY_REQUEST_ERR", 3),
    ("WRONG_DOCUMENT_ERR", 4),
    ("INVALID_CHARACTER_ERR", 5),
    ("NO_DATA_ALLOWED_ERR", 6),
    ("NO_MODIFICATION_ALLOWED_ERR", 7),
    ("NOT_FOUND_ERR", 8),
    ("NOT_SUPPORTED_ERR", 9),
    ("INUSE_ATTRIBUTE_ERR", 10),
    ("INVALID_STATE_ERR", 11),
    ("SYNTAX_ERR", 12),
    ("INVALID_MODIFICATION_ERR", 13),
    ("NAMESPACE_ERR", 14),
    ("INVALID_ACCESS_ERR", 15),
    ("VALIDATION_ERR", 16),
    ("TYPE_MISMATCH_ERR", 17),
    ("SECURITY_ERR", 18),
    ("NETWORK_ERR", 19),
    ("ABORT_ERR", 20),
    ("URL_MISMATCH_ERR", 21),
    ("QUOTA_EXCEEDED_ERR", 22),
    ("TIMEOUT_ERR", 23),
    ("INVALID_NODE_TYPE_ERR", 24),
    ("DATA_CLONE_ERR", 25),
];

/// `WebIDL` §2.8.1's `DOMException` names table, restricted to the rows that
/// carry a legacy code. The names with no code are absent, and §4.4 defines
/// `code` as "0 if no such entry exists in the table", so listing them with a
/// zero would be indistinguishable from a name nobody has heard of - which is
/// the same answer, so the shorter list is the honest one.
const DOM_EXCEPTION_NAME_CODES: [(&str, u16); 22] = [
    ("IndexSizeError", 1),
    ("HierarchyRequestError", 3),
    ("WrongDocumentError", 4),
    ("InvalidCharacterError", 5),
    ("NoModificationAllowedError", 7),
    ("NotFoundError", 8),
    ("NotSupportedError", 9),
    ("InUseAttributeError", 10),
    ("InvalidStateError", 11),
    // Not JavaScript's `SyntaxError`. §2.8.1 says so explicitly: this name
    // reports parsing errors in web APIs - a selector, a date, a colour - while
    // the ECMAScript `SyntaxError` is reserved for the JavaScript parser, and
    // `instanceof SyntaxError` must not become true for a bad selector.
    ("SyntaxError", 12),
    ("InvalidModificationError", 13),
    ("NamespaceError", 14),
    ("InvalidAccessError", 15),
    ("TypeMismatchError", 17),
    ("SecurityError", 18),
    ("NetworkError", 19),
    ("AbortError", 20),
    ("URLMismatchError", 21),
    ("QuotaExceededError", 22),
    ("TimeoutError", 23),
    ("InvalidNodeTypeError", 24),
    ("DataCloneError", 25),
];

/// The legacy code for a `DOMException` name, or `0` for a name §2.8.1 does not
/// list.
pub(crate) fn dom_exception_code(name: &str) -> u16 {
    DOM_EXCEPTION_NAME_CODES
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map_or(0, |(_, code)| *code)
}

/// The behavior of one lazy iterator helper.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum IteratorHelperKind {
    /// `Iterator.from` wrapper: `next` and `return` are forwarded, with no state machine.
    Wrap,
    Map,
    Filter,
    /// `take`: `counter` is the remaining count.
    Take,
    /// `drop`: `counter` is the number of values still to skip.
    Drop,
    FlatMap,
    /// `Iterator.concat`: `buffer` holds (iterable, open method) pairs and `counter` the next pair.
    Concat,
    Chunks(u64),
    /// `windows`: the window size, and whether a short first window is yielded (`allow-partial`).
    Windows(u64, bool),
    /// `Iterator.zip` (`false`) and `Iterator.zipKeyed` (`true`): `buffer` holds four slots per
    /// input (iterator or null when done, next, padding, key).
    Zip(ZipMode, bool),
}

/// How `Iterator.zip` and `Iterator.zipKeyed` treat inputs that finish at different times.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ZipMode {
    Shortest,
    Longest,
    Strict,
}

/// Whether a disposable stack is a `DisposableStack` (sync disposal) or an
/// `AsyncDisposableStack` (awaited disposal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisposeStackKind {
    Sync,
    Async,
}

impl DisposeStackKind {
    pub(crate) const fn is_async(self) -> bool {
        matches!(self, Self::Async)
    }
}

/// The methods of the two stack interfaces. `Dispose` is `dispose` on a
/// `DisposableStack` and `disposeAsync` on an `AsyncDisposableStack`; `Disposed`
/// is the `disposed` getter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisposeStackOp {
    Adopt,
    Defer,
    Dispose,
    Disposed,
    Move,
    Use,
}

/// One entry of a stack's `[[DisposeCapability]]`. `value` is the resource
/// `Dispose` passes as `this` (`undefined` for `defer` and `adopt`), and
/// `method` is what it calls, or `None` for a `null` or `undefined` resource.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DisposeResource {
    pub(crate) value: JsValue,
    pub(crate) method: Option<ObjectId>,
}

/// The progress of an `AsyncDisposableStack.prototype.disposeAsync` that is
/// waiting on an `await`: the throw completion so far, and the promise the
/// call returned.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingDisposal {
    pub(crate) completion: Option<JsValue>,
    pub(crate) promise: usize,
}

/// The internal slots of a `DisposableStack` or `AsyncDisposableStack`:
/// `[[DisposableState]]` (`disposed`), `[[DisposeCapability]]` (`resources`, in
/// insertion order) and the in-flight `disposeAsync` (`disposal`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DisposeStackState {
    pub(crate) kind: DisposeStackKind,
    pub(crate) disposed: bool,
    pub(crate) resources: Vec<DisposeResource>,
    pub(crate) disposal: Option<PendingDisposal>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum ObjectHost {
    #[default]
    Ordinary,
    Array,
    Document(NodeId),
    Node(NodeId),
    ClassList(NodeId),
    /// `element.dataset` `DOMStringMap`. Reads and writes map camelCase
    /// members to `data-*` attributes on the owning element.
    DataSet(NodeId),
    CssStyleDeclaration(NodeId),
    NativeFunction(NativeFunction),
    BoundFunction {
        function: NativeFunction,
        receiver: ObjectId,
    },
    BoundCallable {
        target: ObjectId,
        receiver: JsValue,
        arguments: Vec<JsValue>,
    },
    UserFunction(usize),
    ArrowFunction(usize),
    PromiseConstructor,
    AggregateErrorConstructor,
    ObjectConstructor,
    FunctionConstructor,
    StringConstructor,
    NumberConstructor,
    BigIntConstructor,
    BooleanConstructor,
    DateConstructor,
    SymbolConstructor,
    SymbolInstance(JsSymbol),
    BigIntPrimitive(JsBigInt),
    ArrayConstructor,
    StringPrimitive(String),
    NumberPrimitive(f64),
    BooleanPrimitive(bool),
    DateInstance(f64),
    NamedNodeMap(NodeId),
    Attr {
        owner: NodeId,
        name: String,
    },
    RegExp(usize),
    RegExpConstructor,
    EventConstructor,
    DomConstructor,
    ImageConstructor,
    IntersectionObserverConstructor,
    IntersectionObserver {
        callback: ObjectId,
        targets: Vec<NodeId>,
    },
    MutationObserverConstructor,
    MutationObserver {
        callback: ObjectId,
        targets: Vec<MutationWatch>,
        /// Journal records accumulated since the last delivery, drained by
        /// the microtask that invokes `callback` with the record list.
        queued: Vec<render_dom::MutationRecord>,
    },
    Location(Url),
    ErrorConstructor(ErrorKind),
    /// An error instance; the stand-in for the spec's `[[ErrorData]]` slot.
    ErrorInstance,
    /// `SuppressedError` (ECMA-262 explicit resource management).
    SuppressedErrorConstructor,
    /// `DisposableStack` or `AsyncDisposableStack`, by kind.
    DisposableStackConstructor(DisposeStackKind),
    /// A `DisposableStack` or `AsyncDisposableStack` instance.
    DisposableStack(DisposeStackState),
    /// The `onFulfilled` (`rejected == false`) or `onRejected` callback of an
    /// `await` inside `disposeAsync` on `stack`: it resumes the disposal.
    DisposeAwaitResume {
        stack: ObjectId,
        rejected: bool,
    },
    /// The `DOMException` constructor object (`WebIDL` §4.4).
    DomExceptionConstructor,
    /// defines: its name and its message. `code` is *derived* from the name by
    /// §4.4's "code getter steps ... the legacy code indicated in the
    /// `DOMException` names table for this's name", so it is not stored: a
    /// `DOMException` whose name is not in that table reports `0`, and storing a
    /// third field would let the two disagree.
    DomException {
        name: String,
        message: String,
    },
    /// One `MediaQueryList` (CSSOM View). `media` is the query as
    /// `matchMedia` was given it and `matches` is its **last evaluated**
    /// value, which is what makes the `change` event a diff rather than a
    /// re-read: `queue_media_query_list_changes` compares the new evaluation
    /// against this field and fires only on a flip.
    MediaQueryList {
        media: String,
        matches: bool,
        /// The `change` listeners registered through `addEventListener`. Stored
        /// on the host rather than in a side table because a `MediaQueryList` is
        /// an `EventTarget` that is not a node, and the node-keyed
        /// `event_listeners` map cannot hold it. `onchange` is *not* here: it
        /// is an ordinary own data property, because CSSOM declares it as an
        /// `EventHandler` attribute and script reads and writes it directly.
        listeners: Vec<ObjectId>,
    },
    Promise(usize),
    /// A resolve (`fulfilled`) or reject function of a promise. `pair` indexes
    /// the `alreadyResolved` record the pair shares with its sibling function.
    PromiseSettler {
        promise: usize,
        fulfilled: bool,
        pair: usize,
    },
    /// `Text`, `Comment` and `DocumentFragment`: the DOM interfaces a script can
    /// construct with `new`.
    DomNodeConstructor(DomNodeKind),
    /// A generator object; the index names its coroutine.
    Generator(usize),
    /// The callback an `await` registers on a promise to continue its
    /// coroutine with the settled value (or reason, when `rejected`).
    AsyncResume {
        coroutine: usize,
        rejected: bool,
    },
    /// An async generator object; the index names its coroutine.
    AsyncGenerator(usize),
    /// An async-from-sync iterator (ECMA-262 27.1.4): the sync iterator and its
    /// `next` method, captured when the wrapper was created.
    AsyncFromSyncIterator {
        iterator: ObjectId,
        next: ObjectId,
    },
    /// The `onFulfilled` callback of an async-from-sync step: resolves with the
    /// unwrapped value as an iterator result carrying `done`.
    AsyncFromSyncValue {
        done: bool,
    },
    /// The `onRejected` callback of an async-from-sync step: closes the sync
    /// iterator, then rethrows the reason.
    AsyncFromSyncClose {
        iterator: ObjectId,
    },
    CollectionConstructor(CollectionKind),
    Collection {
        kind: CollectionKind,
        entries: Vec<(JsValue, JsValue)>,
    },
    /// A live `Array` or `TypedArray` iterator (ECMA-262 23.1.5
    /// `CreateArrayIterator`). Each step reads the target's length, so a write or
    /// resize during iteration is seen. `target` is `None` once exhausted.
    ArrayIterator {
        target: Option<ObjectId>,
        index: usize,
        view: ArrayView,
    },
    CollectionIterator {
        values: Vec<JsValue>,
        index: usize,
    },
    /// A `%RegExpStringIterator%` (ECMA-262 22.2.9): `matcher` is the `RegExp`
    /// that `matchAll` cloned, stepped by `next()` over `input`.
    RegExpStringIterator {
        matcher: ObjectId,
        input: String,
        global: bool,
        unicode: bool,
        done: bool,
    },
    /// A lazy iterator helper (`%IteratorPrototype%.map/take/chunks/...`).
    /// The state machine is stepped one `next()` call at a time by
    /// [`crate::runtime::builtins::iterator`].
    IteratorHelper {
        kind: IteratorHelperKind,
        /// The underlying iterator and its `next` method.
        source: Option<ObjectId>,
        source_next: Option<ObjectId>,
        /// The callback carried by map/filter/flatMap/reduce/...
        callback: Option<ObjectId>,
        /// A helper-specific inner iterator (flatMap, concat, windows).
        inner: Option<ObjectId>,
        inner_next: Option<ObjectId>,
        /// Generic per-helper counter (`take`/`drop`/`chunks`/`windows`).
        counter: u64,
        /// Per-helper buffer (concat pairs, chunks, windows).
        buffer: Vec<JsValue>,
        done: bool,
        /// Set while a `next`/`return` resumption runs (the generator "executing" state).
        running: bool,
        /// Set once `next` has run, so `return` can tell "suspended-start" from "suspended-yield".
        started: bool,
    },
    TypedArrayConstructor(TypedArrayKind),
    TypedArray {
        kind: TypedArrayKind,
        buffer: TypedBuffer,
        /// Element offset of this view within the shared buffer.
        start: usize,
        /// Element count of this view, or `None` for a length-tracking view of
        /// a resizable buffer, whose count follows the buffer's length.
        length: Option<usize>,
    },
    /// A `DataView` over a byte-granular shared buffer. `byte_offset` and
    /// `byte_length` are byte positions. There is deliberately no endianness
    /// field: ECMAScript 25.2.5.1 gives the constructor three parameters, so
    /// every accessor carries its own `littleEndian` and the default is
    /// big-endian.
    DataView {
        buffer: TypedBuffer,
        /// Byte offset of the view within the shared buffer.
        byte_offset: usize,
        /// Byte length of the view, or `None` for a length-tracking view.
        byte_length: Option<usize>,
    },
    /// An `ArrayBuffer`: the bytes that the typed-array and `DataView` families
    /// view. Every view reads and writes the same store. A `SharedArrayBuffer`
    /// is the same host, told apart by its store.
    ArrayBufferHost(TypedBuffer),
    SharedArrayBufferConstructor,
    TextDecoder {
        encoding: TextEncoding,
        fatal: bool,
        /// A truncated multi-byte sequence held back for the next `decode`
        /// call, and whether a leading BOM has already been consumed.
        pending: Vec<u8>,
        bom_seen: bool,
    },
    TextEncoderConstructor,
    TextDecoderConstructor,
    DataViewConstructor,
    ArrayBufferConstructor,
    UrlConstructor,
    UrlSearchParamsConstructor,
    UrlInstance(Url),
    UrlSearchParams {
        pairs: Vec<(String, String)>,
        owner: Option<ObjectId>,
    },
    /// A Web Storage area (`localStorage`/`sessionStorage`). Its entries are
    /// the object's own string-keyed properties, so the ordinary own-key
    /// machinery already provides insertion order and enumeration.
    Storage,
    /// The `XMLHttpRequest` constructor object.
    XmlHttpRequestConstructor,
    /// One `XMLHttpRequest` instance with its captured request state.
    XmlHttpRequest(XmlHttpRequestState),
    AbortControllerConstructor,
    AbortController,
    AbortSignal,
    FormDataConstructor,
    FormData {
        entries: Vec<(String, String)>,
    },
    /// The `Response` constructor object.
    ResponseConstructor,
    /// One settled `fetch` response.
    Response {
        status: u16,
        status_text: String,
        headers: Vec<(String, String)>,
        /// Body decoded lossily as UTF-8; `text()` and `json()` read this.
        body: String,
    },
    /// A `response.headers` instance reading through its owning `Response`.
    ResponseHeaders {
        owner: ObjectId,
    },
    /// Immutable bytes exposed by the File API `Blob` surface. The runtime
    /// keeps the payload bounded and stores it outside the DOM.
    Blob {
        bytes: Vec<u8>,
        content_type: String,
    },
    BlobConstructor,
    ProxyConstructor,
    /// A Proxy exotic object. `handler` is `None` once the proxy is revoked;
    /// `target` stays so the proxy keeps the callability it was created with.
    Proxy {
        target: ObjectId,
        handler: Option<ObjectId>,
    },
    /// The `Video` (`HTMLVideoElement`) constructor object.
    VideoConstructor,
    /// One `HTMLVideoElement` instance with its playback state. Script
    /// visible fields (`src`, `duration`, ...) are plain properties updated
    /// in place, mirroring the `XMLHttpRequest` pattern.
    VideoElement(VideoElementState),
}

impl ObjectHost {
    /// Whether an object carrying this host is callable, i.e. whether `typeof`
    /// answers `"function"` and a call is attempted rather than refused.
    ///
    /// This is the *single* answer. It used to be written down twice - once
    /// here for `install_builtin_metadata` and once in the interpreter for
    /// `typeof` - and the two copies had drifted in the same direction, which
    /// is how four installed constructors came to report `typeof` `"object"`.
    /// One function cannot drift from itself, so this is the one.
    pub(crate) const fn is_callable(&self) -> bool {
        matches!(
            self,
            Self::NativeFunction(_)
                | Self::BoundFunction { .. }
                | Self::BoundCallable { .. }
                | Self::UserFunction(_)
                | Self::ArrowFunction(_)
                | Self::FunctionConstructor
                | Self::StringConstructor
                | Self::NumberConstructor
                | Self::BigIntConstructor
                | Self::BooleanConstructor
                | Self::DateConstructor
                | Self::SymbolConstructor
                | Self::ArrayConstructor
                | Self::RegExpConstructor
                | Self::EventConstructor
                | Self::DomConstructor
                | Self::DomNodeConstructor(_)
                | Self::ImageConstructor
                | Self::VideoConstructor
                | Self::ObjectConstructor
                | Self::PromiseConstructor
                | Self::AggregateErrorConstructor
                | Self::MutationObserverConstructor
                | Self::UrlConstructor
                | Self::UrlSearchParamsConstructor
                | Self::XmlHttpRequestConstructor
                | Self::AbortControllerConstructor
                | Self::FormDataConstructor
                | Self::ResponseConstructor
                | Self::BlobConstructor
                | Self::ProxyConstructor
                | Self::IntersectionObserverConstructor
                | Self::CollectionConstructor(_)
                | Self::TypedArrayConstructor(_)
                | Self::ErrorConstructor(_)
                | Self::DomExceptionConstructor
                | Self::ArrayBufferConstructor
                | Self::SharedArrayBufferConstructor
                | Self::DataViewConstructor
                | Self::TextEncoderConstructor
                | Self::TextDecoderConstructor
                | Self::PromiseSettler { .. }
                | Self::AsyncResume { .. }
                | Self::AsyncFromSyncValue { .. }
                | Self::AsyncFromSyncClose { .. }
                | Self::DisposeAwaitResume { .. }
                | Self::SuppressedErrorConstructor
                | Self::DisposableStackConstructor(_)
        )
    }
}

/// Playback machinery of one `HTMLVideoElement` instance.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct VideoElementState {
    /// Absolute `src` of the currently queued or loaded media, when set.
    pub(super) resolved_src: Option<Url>,
    /// Id of the media transfer queued in `pending_fetch_requests` until the
    /// embedding settles it through `settle_video_fetch`.
    pub(super) load_id: Option<u64>,
    /// Bumped whenever a new load starts so stale settlements can be
    /// recognized and dropped.
    pub(super) generation: u64,
    /// Demux/decode pipeline once a media load succeeded.
    pub(super) media: Option<VideoMedia>,
    /// `play()` promises awaiting media readiness, resolved (or rejected)
    /// when the pending load settles. GC roots through `mark_host`.
    pub(super) pending_play_promises: Vec<VideoPlayPromise>,
}

/// One `play()` promise awaiting media readiness: the promise record index
/// used to settle it, and the object id the GC must keep alive until then.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VideoPlayPromise {
    pub(super) record: usize,
    pub(super) object: ObjectId,
}

/// Shared handle to the demux/decode pipeline of a loaded video. Identity
/// (not content) equality keeps `ObjectHost` comparisons cheap.
#[derive(Clone)]
pub(crate) struct VideoMedia(std::rc::Rc<std::cell::RefCell<crate::video::VideoPipeline>>);

impl VideoMedia {
    pub(super) fn new(pipeline: crate::video::VideoPipeline) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(pipeline)))
    }

    /// Access the underlying pipeline.
    #[must_use]
    pub fn pipeline(&self) -> &std::rc::Rc<std::cell::RefCell<crate::video::VideoPipeline>> {
        &self.0
    }
}

impl fmt::Debug for VideoMedia {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("VideoMedia")
            .field(&self.0.borrow().track().info)
            .finish()
    }
}

impl PartialEq for VideoMedia {
    fn eq(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.0, &other.0)
    }
}

/// Mutable request state of one `XMLHttpRequest` instance. The classic
/// subset keeps the request line, caller headers, and the settle-time
/// response; script-visible fields (`readyState`, `status`, ...) are plain
/// properties the completion path updates.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct XmlHttpRequestState {
    /// Uppercased request method. Empty means `open()` has not run yet.
    pub(super) method: String,
    /// Absolute request URL resolved against the document base at `open()`.
    pub(super) url: String,
    /// Headers accumulated by `setRequestHeader()` before `send()`.
    pub(super) headers: Vec<(String, String)>,
    /// `false` only when `open()` received an explicit falsy async flag;
    /// synchronous sends are rejected.
    pub(super) async_request: bool,
    /// `true` once `send()` queued the network request.
    pub(super) sent: bool,
    /// Response captured when the embedding settles the transfer.
    pub(super) response: Option<XhrResponse>,
}

/// Settle-time response state of one `XMLHttpRequest` instance.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct XhrResponse {
    pub(super) status: u16,
    pub(super) status_text: String,
    pub(super) headers: Vec<(String, String)>,
    /// Body decoded lossily as UTF-8.
    pub(super) body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags mirror MutationObserverInit's independent boolean options"
)]
pub(crate) struct MutationWatch {
    pub target: NodeId,
    pub subtree: bool,
    pub child_list: bool,
    pub attributes: bool,
    pub character_data: bool,
}

/// An object stored in a realm. Host identity is intentionally private: DOM
/// wrappers can only be created by the binding layer.
#[derive(Clone, Debug)]
pub struct JsObject {
    properties: BTreeMap<String, PropertyDescriptor>,
    /// Symbol-keyed properties, keyed by symbol id with the owning symbol
    /// kept alongside so descriptions survive (`getOwnPropertySymbols`).
    symbols: BTreeMap<u64, (JsSymbol, PropertyDescriptor)>,
    /// String keys in first-insertion order (spec own-key ordering pairs
    /// this with ascending integer indices). Keys absent from the list
    /// (bootstrap-installed builtins) enumerate in map order after it.
    key_order: Vec<String>,
    prototype: Option<ObjectId>,
    pub(crate) host: ObjectHost,
    /// `Object.preventExtensions` and friends; every object starts extensible.
    extensible: bool,
    /// Own private field values, keyed by the class-unique private-name id.
    /// Presence is the brand check for `#field in object`.
    private_fields: BTreeMap<u64, JsValue>,
    /// Private methods and accessors installed on this object (class
    /// prototypes and constructors). Instance lookup walks the prototype
    /// chain, mirroring how private methods are reachable from derived code.
    private_methods: BTreeMap<u64, PropertyDescriptor>,
}

impl Default for JsObject {
    fn default() -> Self {
        Self {
            properties: BTreeMap::new(),
            symbols: BTreeMap::new(),
            key_order: Vec::new(),
            prototype: None,
            host: ObjectHost::default(),
            extensible: true,
            private_fields: BTreeMap::new(),
            private_methods: BTreeMap::new(),
        }
    }
}

impl JsObject {
    #[must_use]
    pub fn own_property(&self, key: &str) -> Option<&PropertyDescriptor> {
        self.properties.get(key)
    }

    #[must_use]
    pub const fn prototype(&self) -> Option<ObjectId> {
        self.prototype
    }

    /// Own data properties in stable insertion order (`BTreeMap` key order).
    /// Every object id reachable through this object's properties: data
    /// values plus accessor getter/setter slots, which the collector must
    /// treat as strong references just like values.
    pub(crate) fn property_object_references(&self) -> Vec<ObjectId> {
        let mut references = Vec::new();
        for descriptor in self
            .properties
            .values()
            .chain(self.symbols.values().map(|(_, descriptor)| descriptor))
            .chain(self.private_methods.values())
        {
            if descriptor.is_accessor() {
                references.extend(descriptor.getter);
                references.extend(descriptor.setter);
            } else if let JsValue::Object(id) = &descriptor.value {
                references.push(*id);
            }
        }
        for value in self.private_fields.values() {
            if let JsValue::Object(id) = value {
                references.push(*id);
            }
        }
        references
    }

    /// Define (or overwrite) an own private field value.
    pub(crate) fn set_private_field(&mut self, id: u64, value: JsValue) {
        self.private_fields.insert(id, value);
    }

    #[must_use]
    pub(crate) fn private_field(&self, id: u64) -> Option<&JsValue> {
        self.private_fields.get(&id)
    }

    #[must_use]
    pub(crate) fn has_private_field(&self, id: u64) -> bool {
        self.private_fields.contains_key(&id)
    }

    pub(crate) fn define_private_method(&mut self, id: u64, descriptor: PropertyDescriptor) {
        self.private_methods.insert(id, descriptor);
    }

    #[must_use]
    pub(crate) fn private_method(&self, id: u64) -> Option<&PropertyDescriptor> {
        self.private_methods.get(&id)
    }
}

/// Global state and object identity for one JavaScript realm.
#[derive(Debug)]
pub struct Realm {
    objects: Vec<JsObject>,
    global: ObjectId,
    document: ObjectId,
    object_prototype: ObjectId,
    function_prototype: ObjectId,
    array_prototype: ObjectId,
    string_prototype: ObjectId,
    number_primitive_prototype: ObjectId,
    boolean_primitive_prototype: ObjectId,
    regexp_prototype: ObjectId,
    date_prototype: ObjectId,
    symbol_prototype: ObjectId,
    /// `%BigInt.prototype%`, the prototype of every `BigInt` wrapper.
    bigint_prototype: ObjectId,
    /// `%Promise%`, the intrinsic constructor `SpeciesConstructor` falls back to.
    promise_constructor: ObjectId,
    promise_prototype: ObjectId,
    element_prototype: ObjectId,
    /// The prototype of every DOM interface, by interface name.
    dom_prototypes: BTreeMap<&'static str, ObjectId>,
    /// `%IteratorPrototype%` carrying the iterator-helper methods.
    iterator_prototype: ObjectId,
    /// `%RegExpStringIteratorPrototype%`: the `next` of `matchAll`'s iterator.
    regexp_string_iterator_prototype: ObjectId,
    /// `%GeneratorPrototype%`: `next`, `return` and `throw` of every generator.
    generator_prototype: ObjectId,
    /// `%AsyncGeneratorPrototype%` (ECMA-262 27.6.1).
    async_generator_prototype: ObjectId,
    /// `%AsyncGeneratorFunction.prototype%` (ECMA-262 27.7.1): the prototype of
    /// every async generator function.
    async_generator_function_prototype: ObjectId,
    /// `%AsyncFromSyncIteratorPrototype%` (ECMA-262 27.1.4).
    async_from_sync_iterator_prototype: ObjectId,
    /// `%Storage.prototype%` shared by `localStorage` and `sessionStorage`.
    storage_prototype: ObjectId,
    /// `%DisposableStack.prototype%`, `%AsyncDisposableStack.prototype%` and
    /// `%SuppressedError.prototype%`: the intrinsics `move` and disposal create
    /// instances from, whatever `this` or `new.target` is.
    disposable_stack_prototype: ObjectId,
    async_disposable_stack_prototype: ObjectId,
    suppressed_error_prototype: ObjectId,
    /// `%MediaQueryList.prototype%`. Root it explicitly: a script that drops
    /// every reference to a list still gets a live list that must keep
    /// evaluating, so the prototype outlives the lists too.
    media_query_list_prototype: ObjectId,
    /// `%IteratorHelperPrototype%` shared by helper result objects.
    iterator_helper_prototype: ObjectId,
    /// `%WrapForValidIteratorPrototype%`: the prototype of `Iterator.from` wrappers.
    iterator_wrap_prototype: ObjectId,
    node_wrappers: BTreeMap<NodeId, ObjectId>,
    class_list_wrappers: BTreeMap<NodeId, ObjectId>,
    style_declaration_wrappers: BTreeMap<NodeId, ObjectId>,
    dataset_wrappers: BTreeMap<NodeId, ObjectId>,
    /// Number of object slots that became garbage and were swept. Object
    /// identities are never moved or reused, so a swept slot always reads as
    /// an empty ordinary object even if some bookkeeping still references it.
    swept_objects: usize,
}

impl Realm {
    #[allow(
        clippy::too_many_lines,
        reason = "bootstrap installs every builtin in one explicit sequence"
    )]
    pub(crate) fn bootstrap(document_node: NodeId, document_url: &Url) -> Self {
        let mut objects = vec![JsObject::default()];
        let global = ObjectId(0);
        objects.push(JsObject {
            host: ObjectHost::Document(document_node),
            ..JsObject::default()
        });
        let document = ObjectId(1);
        objects[global.0].properties.insert(
            "document".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(document),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        for (name, value) in [
            ("NaN", JsValue::Number(f64::NAN)),
            ("Infinity", JsValue::Number(f64::INFINITY)),
            ("undefined", JsValue::Undefined),
        ] {
            objects[global.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value,
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
        }
        let queue_microtask = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::BoundFunction {
                function: NativeFunction::QueueMicrotask,
                receiver: global,
            },
            ..JsObject::default()
        });
        objects[global.0].properties.insert(
            "queueMicrotask".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(queue_microtask),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        let object_prototype = Self::install_object(&mut objects, global);
        let function_prototype = Self::install_function(&mut objects, global, object_prototype);
        // §20.1.1: `Object` is a built-in function, so its [[Prototype]] is
        // `%Function.prototype%`; the constructor exists only once `install_object` ran.
        if let Some(JsValue::Object(object_constructor)) = objects[global.0]
            .properties
            .get("Object")
            .map(|descriptor| descriptor.value.clone())
        {
            objects[object_constructor.0].prototype = Some(function_prototype);
        }
        let (element_prototype, dom_prototypes) = Self::install_dom_interfaces(
            &mut objects,
            global,
            object_prototype,
            function_prototype,
        );
        // DOM libraries feature-detect and use `instanceof Document` during
        // startup. Keep a dedicated prototype for the live document wrapper
        // so the check has the same result as a browser while the constructor
        // remains intentionally non-constructible.
        let document_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(element_prototype),
            ..JsObject::default()
        });
        let document_constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::DomConstructor,
            ..JsObject::default()
        });
        objects[document_constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(document_prototype)),
        );
        objects[document_prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(document_constructor)),
        );
        objects[document.0].prototype = Some(document_prototype);
        for name in ["Document", "HTMLDocument"] {
            objects[global.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(document_constructor),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
        Self::install_location(
            &mut objects,
            global,
            document,
            object_prototype,
            function_prototype,
            document_url,
        );
        Self::install_navigator(&mut objects, global, object_prototype);
        Self::install_screen(&mut objects, global, object_prototype);
        Self::install_performance(&mut objects, global, object_prototype, function_prototype);
        let error_prototype =
            Self::install_errors(&mut objects, global, object_prototype, function_prototype);
        Self::install_dom_exception(&mut objects, global, error_prototype, function_prototype);
        Self::install_event(&mut objects, global, object_prototype, function_prototype);
        let string_prototype =
            Self::install_string(&mut objects, global, object_prototype, function_prototype);
        let regexp_prototype =
            Self::install_regexp(&mut objects, global, object_prototype, function_prototype);

        let number_primitive_prototype =
            Self::install_number(&mut objects, global, object_prototype, function_prototype);
        let boolean_primitive_prototype =
            Self::install_boolean(&mut objects, global, object_prototype, function_prototype);
        let date_prototype =
            Self::install_date(&mut objects, global, object_prototype, function_prototype);
        let symbol_prototype =
            Self::install_symbol(&mut objects, global, object_prototype, function_prototype);
        let bigint_prototype =
            Self::install_bigint(&mut objects, global, object_prototype, function_prototype);
        Self::install_math(&mut objects, global, object_prototype);
        let (promise_constructor, promise_prototype) = Self::install_promise(
            &mut objects,
            global,
            object_prototype,
            function_prototype,
            error_prototype,
        );
        let suppressed_error_prototype = Self::install_suppressed_error(
            &mut objects,
            global,
            function_prototype,
            error_prototype,
        );
        let disposable_stack_prototype = Self::install_disposable_stack(
            &mut objects,
            global,
            object_prototype,
            function_prototype,
            DisposeStackKind::Sync,
        );
        let async_disposable_stack_prototype = Self::install_disposable_stack(
            &mut objects,
            global,
            object_prototype,
            function_prototype,
            DisposeStackKind::Async,
        );
        let array_prototype =
            Self::install_array(&mut objects, global, object_prototype, function_prototype);
        let (iterator_prototype, iterator_helper_prototype, iterator_wrap_prototype) =
            Self::install_iterator(&mut objects, global, object_prototype, function_prototype);
        let regexp_string_iterator_prototype = Self::install_regexp_string_iterator(
            &mut objects,
            iterator_prototype,
            function_prototype,
        );
        let generator_prototype =
            Self::install_generator(&mut objects, function_prototype, iterator_prototype);
        let (
            async_generator_prototype,
            async_from_sync_iterator_prototype,
            async_generator_function_prototype,
        ) = Self::install_async_iteration(&mut objects, object_prototype, function_prototype);
        let storage_prototype =
            Self::install_storage(&mut objects, global, object_prototype, function_prototype);
        for name in ["localStorage", "sessionStorage"] {
            Self::install_storage_area(&mut objects, global, storage_prototype, name);
        }
        Self::install_collections(&mut objects, global, object_prototype, function_prototype);
        Self::install_typed_arrays(&mut objects, global, object_prototype, function_prototype);
        Self::install_encoding(&mut objects, global, object_prototype, function_prototype);
        Self::install_json(&mut objects, global, object_prototype, function_prototype);
        Self::install_fetch(&mut objects, global, object_prototype, function_prototype);
        Self::install_proxy_reflect(&mut objects, global, object_prototype, function_prototype);
        Self::install_video(&mut objects, global, object_prototype, function_prototype);
        Self::define_global_function(
            &mut objects,
            global,
            "getComputedStyle",
            NativeFunction::GetComputedStyle,
        );
        for (name, function) in [
            ("parseInt", NativeFunction::GlobalParseInt),
            ("parseFloat", NativeFunction::GlobalParseFloat),
            ("isNaN", NativeFunction::GlobalIsNaN),
            ("isFinite", NativeFunction::GlobalIsFinite),
            ("encodeURI", NativeFunction::GlobalEncodeURI),
            (
                "encodeURIComponent",
                NativeFunction::GlobalEncodeURIComponent,
            ),
            ("decodeURI", NativeFunction::GlobalDecodeURI),
            (
                "decodeURIComponent",
                NativeFunction::GlobalDecodeURIComponent,
            ),
            ("escape", NativeFunction::GlobalEscape),
            ("unescape", NativeFunction::GlobalUnescape),
            ("atob", NativeFunction::GlobalAtob),
            ("btoa", NativeFunction::GlobalBtoa),
            ("__render_random_bytes", NativeFunction::GlobalRandomBytes),
            ("eval", NativeFunction::GlobalEvalStub),
            ("__render_noop", NativeFunction::GlobalNoop),
        ] {
            Self::define_global_function(&mut objects, global, name, function);
        }

        // `import()` is callable in module scripts and also carries the
        // standard `import.meta` object. Resolution is delegated to the
        // embedding, so the runtime returns an already-fulfilled namespace
        // placeholder instead of throwing during feature detection.
        let dynamic_import = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::BoundFunction {
                function: NativeFunction::GlobalImport,
                receiver: global,
            },
            ..JsObject::default()
        });
        let import_meta = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        objects[import_meta.0].properties.insert(
            "url".to_owned(),
            PropertyDescriptor::builtin(JsValue::String(document_url.to_string())),
        );
        objects[dynamic_import.0].properties.insert(
            "meta".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(import_meta)),
        );
        // The source-phase proposals' `import.defer(...)` and `import.source(...)`
        // reach the same host hook as `import()`, so their arguments evaluate
        // before the call like any other call's.
        for phase in ["defer", "source"] {
            let phase_import = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::BoundFunction {
                    function: NativeFunction::GlobalImport,
                    receiver: global,
                },
                ..JsObject::default()
            });
            objects[dynamic_import.0].properties.insert(
                phase.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(phase_import)),
            );
        }
        objects[global.0].properties.insert(
            "import".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(dynamic_import),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );

        // Session history is owned by the browser shell. These methods record
        // a `HistoryRequest` the shell applies after the turn, and
        // `pushState`/`replaceState` move the location at once, so routing code
        // sees the new URL without a load.
        let history = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("back", NativeFunction::HistoryBack),
            ("forward", NativeFunction::HistoryForward),
            ("go", NativeFunction::HistoryGo),
            ("pushState", NativeFunction::HistoryPushState),
            ("replaceState", NativeFunction::HistoryReplaceState),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[history.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[history.0].properties.insert(
            "length".to_owned(),
            PropertyDescriptor::builtin(JsValue::Number(1.0)),
        );
        objects[history.0].properties.insert(
            "state".to_owned(),
            PropertyDescriptor::builtin(JsValue::Null),
        );
        objects[global.0].properties.insert(
            "history".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(history)),
        );

        let system = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        objects[system.0].properties.insert(
            "import".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(dynamic_import)),
        );
        objects[global.0].properties.insert(
            "System".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(system)),
        );

        // Every callable native object inherits Function.prototype.  A number
        // of older installers predate the shared function prototype and left
        // their methods as prototype-less host objects.  Real-world shims use
        // patterns such as `Array.prototype.slice.call(...)` and
        // `fn.apply(...)` during bootstrap, so repair the invariant in one
        // place instead of relying on each installer to remember it.
        for object in &mut objects {
            if matches!(
                &object.host,
                ObjectHost::NativeFunction(_)
                    | ObjectHost::BoundFunction { .. }
                    | ObjectHost::BoundCallable { .. }
                    | ObjectHost::PromiseSettler { .. }
                    | ObjectHost::AsyncResume { .. }
                    | ObjectHost::AsyncFromSyncValue { .. }
                    | ObjectHost::AsyncFromSyncClose { .. }
                    | ObjectHost::DisposeAwaitResume { .. }
            ) && object.prototype.is_none()
            {
                object.prototype = Some(function_prototype);
            }
        }
        // Browser constructors used by page bootstrap and resource discovery.
        let image = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::ImageConstructor,
            ..JsObject::default()
        });
        objects[image.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(element_prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );

        let css = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let css_supports = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::CssSupports),
            ..JsObject::default()
        });
        objects[css.0].properties.insert(
            "supports".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(css_supports)),
        );
        objects[global.0].properties.insert(
            "CSS".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(css)),
        );

        // URL and URLSearchParams are small but foundational Web APIs.  Many
        // production bundles use them during startup for query routing and
        // telemetry; keeping the objects in the realm also gives ordinary
        // prototype lookup and method calls the same shape as browsers.
        let url_search_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("get", NativeFunction::UrlSearchParamsGet),
            ("has", NativeFunction::UrlSearchParamsHas),
            ("set", NativeFunction::UrlSearchParamsSet),
            ("append", NativeFunction::UrlSearchParamsAppend),
            ("toString", NativeFunction::UrlSearchParamsToString),
            ("forEach", NativeFunction::UrlSearchParamsForEach),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[url_search_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        let url_search_constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::UrlSearchParamsConstructor,
            ..JsObject::default()
        });
        objects[url_search_constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(url_search_prototype)),
        );
        objects[global.0].properties.insert(
            "URLSearchParams".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(url_search_constructor)),
        );

        let url_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let url_to_string = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::UrlToString),
            ..JsObject::default()
        });
        objects[url_prototype.0].properties.insert(
            "toString".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(url_to_string)),
        );
        let url_constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::UrlConstructor,
            ..JsObject::default()
        });
        objects[url_constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(url_prototype)),
        );
        for (name, function) in [
            ("createObjectURL", NativeFunction::UrlCreateObjectUrl),
            ("revokeObjectURL", NativeFunction::UrlRevokeObjectUrl),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[url_constructor.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[global.0].properties.insert(
            "URL".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(url_constructor)),
        );
        objects[global.0].properties.insert(
            "Image".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(image),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        let intersection_observer_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("observe", NativeFunction::IntersectionObserve),
            ("unobserve", NativeFunction::IntersectionUnobserve),
            ("disconnect", NativeFunction::IntersectionDisconnect),
            ("takeRecords", NativeFunction::IntersectionTakeRecords),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[intersection_observer_prototype.0]
                .properties
                .insert(
                    name.to_owned(),
                    PropertyDescriptor::builtin(JsValue::Object(method)),
                );
        }
        let intersection_observer = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::IntersectionObserverConstructor,
            ..JsObject::default()
        });
        objects[intersection_observer.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(intersection_observer_prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[global.0].properties.insert(
            "IntersectionObserver".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(intersection_observer),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        let entry_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, value) in [
            ("intersectionRatio", JsValue::Number(0.0)),
            ("isIntersecting", JsValue::Boolean(false)),
        ] {
            objects[entry_prototype.0]
                .properties
                .insert(name.to_owned(), PropertyDescriptor::builtin(value));
        }
        let entry_constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::DomConstructor,
            ..JsObject::default()
        });
        objects[entry_constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(entry_prototype)),
        );
        objects[global.0].properties.insert(
            "IntersectionObserverEntry".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(entry_constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        let mutation_observer_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("observe", NativeFunction::MutationObserve),
            ("disconnect", NativeFunction::MutationDisconnect),
            ("takeRecords", NativeFunction::MutationTakeRecords),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[mutation_observer_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        let mutation_observer = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::MutationObserverConstructor,
            ..JsObject::default()
        });
        objects[mutation_observer.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(mutation_observer_prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[global.0].properties.insert(
            "MutationObserver".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(mutation_observer),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        // `MediaQueryList` (CSSOM View). The interface is
        // `[Exposed=Window] interface MediaQueryList : EventTarget`, and
        // `matchMedia` is its only producer, so there is no global constructor:
        // a script can obtain one and read its prototype, but cannot
        // `new MediaQueryList(...)`. That is the same shape the platform has,
        // and it is why `MediaQueryList.prototype` is installed here and the
        // constructor is not.
        let media_query_list_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            (
                "addEventListener",
                NativeFunction::MediaQueryListAddEventListener,
            ),
            (
                "removeEventListener",
                NativeFunction::MediaQueryListRemoveEventListener,
            ),
            ("addListener", NativeFunction::MediaQueryListAddListener),
            (
                "removeListener",
                NativeFunction::MediaQueryListRemoveListener,
            ),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[media_query_list_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        // `media` and `matches` are WebIDL §2.5.2 `readonly attribute`s, so
        // they are accessors on the prototype reading internal slots, not own
        // data properties on the list. That is not a formality here: `matches`
        // changes when the viewport does, and an own data property would have to
        // be rewritten on every frame, which is exactly the kind of thing that
        // silently goes stale. Reading through the accessor cannot.
        for (name, getter) in [
            ("media", NativeFunction::MediaQueryListMediaGetter),
            ("matches", NativeFunction::MediaQueryListMatchesGetter),
        ] {
            let accessor = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(getter),
                ..JsObject::default()
            });
            objects[media_query_list_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    value: JsValue::Undefined,
                    writable: false,
                    getter: Some(accessor),
                    setter: None,
                    enumerable: true,
                    configurable: true,
                },
            );
        }
        objects[media_query_list_prototype.0].properties.insert(
            "onchange".to_owned(),
            // CSSOM declares `onchange` as an `EventHandler` *attribute*, so it
            // is a writable data property rather than an accessor, and it starts
            // `null`.
            PropertyDescriptor::builtin(JsValue::Null),
        );
        // WebIDL §2.7.2: an interface prototype's `@@toStringTag` is the interface
        // name, which is what a polyfill branching on
        // `Object.prototype.toString.call(mql)` reads.
        let media_query_list_tag = JsSymbol::well_known("@@toStringTag");
        objects[media_query_list_prototype.0].symbols.insert(
            media_query_list_tag.id(),
            (
                media_query_list_tag,
                PropertyDescriptor::builtin(JsValue::String("MediaQueryList".to_owned())),
            ),
        );
        // The interface object exists even though the interface has no
        // constructor operation: CSSOM View declares
        // `[Exposed=Window] interface MediaQueryList : EventTarget`, and an
        // exposed interface gets a global object, so `mql instanceof
        // MediaQueryList` and `Object.getPrototypeOf(mql) ===
        // MediaQueryList.prototype` both work. `new MediaQueryList()` is a
        // `TypeError`, as it is in a browser.
        let media_query_list_constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            // `EventTarget` is a non-constructible interface and so is
            // `MediaQueryList`'s own constructor operation being absent; the
            // engine models the "illegal constructor" case with the same host the
            // DOM interface uses, because the observable behaviour is identical.
            host: ObjectHost::DomConstructor,
            ..JsObject::default()
        });
        objects[media_query_list_constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(media_query_list_prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[media_query_list_prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(media_query_list_constructor)),
        );
        objects[global.0].properties.insert(
            "MediaQueryList".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(media_query_list_constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        Self::define_global_function(
            &mut objects,
            global,
            "matchMedia",
            NativeFunction::WindowMatchMedia,
        );
        Self::install_console(&mut objects, global);
        Self::install_timers(&mut objects, global);
        for (index, object) in objects.iter_mut().enumerate() {
            if object.prototype.is_none() {
                object.prototype = match &object.host {
                    ObjectHost::NativeFunction(_)
                    | ObjectHost::BoundFunction { .. }
                    | ObjectHost::BoundCallable { .. }
                    | ObjectHost::UserFunction(_)
                    | ObjectHost::ArrowFunction(_)
                    | ObjectHost::FunctionConstructor
                    | ObjectHost::StringConstructor
                    | ObjectHost::NumberConstructor
                    | ObjectHost::BigIntConstructor
                    | ObjectHost::BooleanConstructor
                    | ObjectHost::DateConstructor
                    | ObjectHost::SymbolConstructor
                    | ObjectHost::ArrayConstructor
                    | ObjectHost::RegExpConstructor
                    | ObjectHost::EventConstructor
                    | ObjectHost::DomConstructor
                    | ObjectHost::DomNodeConstructor(_)
                    | ObjectHost::ImageConstructor
                    | ObjectHost::VideoConstructor
                    | ObjectHost::IntersectionObserverConstructor
                    | ObjectHost::MutationObserverConstructor
                    | ObjectHost::ErrorConstructor(_)
                    | ObjectHost::PromiseSettler { .. }
                    | ObjectHost::AsyncResume { .. }
                    | ObjectHost::AsyncFromSyncValue { .. }
                    | ObjectHost::AsyncFromSyncClose { .. }
                    | ObjectHost::DisposeAwaitResume { .. }
                    | ObjectHost::SuppressedErrorConstructor
                    | ObjectHost::DisposableStackConstructor(_)
                    | ObjectHost::CollectionConstructor(_) => Some(function_prototype),
                    _ if index != object_prototype.0 => Some(object_prototype),
                    _ => None,
                };
            }
        }
        // `Array.prototype[Symbol.unscopables]` has no prototype at all, which the
        // loop above would have replaced with `Object.prototype`.
        let unscopables = JsSymbol::well_known("@@unscopables");
        if let Some((_, descriptor)) = objects[array_prototype.0].symbols.get(&unscopables.id())
            && let JsValue::Object(unscopables) = descriptor.value
        {
            objects[unscopables.0].prototype = None;
        }
        Self::install_builtin_metadata(&mut objects);
        Self {
            objects,
            global,
            document,
            object_prototype,
            function_prototype,
            array_prototype,
            string_prototype,
            number_primitive_prototype,
            boolean_primitive_prototype,
            regexp_prototype,
            date_prototype,
            symbol_prototype,
            promise_constructor,
            bigint_prototype,
            promise_prototype,
            element_prototype,
            dom_prototypes,
            iterator_prototype,
            regexp_string_iterator_prototype,
            generator_prototype,
            async_generator_prototype,
            async_generator_function_prototype,
            async_from_sync_iterator_prototype,
            iterator_helper_prototype,
            iterator_wrap_prototype,
            storage_prototype,
            disposable_stack_prototype,
            async_disposable_stack_prototype,
            suppressed_error_prototype,
            media_query_list_prototype,
            node_wrappers: BTreeMap::new(),
            class_list_wrappers: BTreeMap::new(),
            style_declaration_wrappers: BTreeMap::new(),
            dataset_wrappers: BTreeMap::new(),
            swept_objects: 0,
        }
    }

    fn define_global_function(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        name: &str,
        function: NativeFunction,
    ) {
        let callable = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::BoundFunction {
                function,
                receiver: global,
            },
            ..JsObject::default()
        });
        objects[global.0].properties.insert(
            name.to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(callable),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// Build the DOM interface objects with their real inheritance:
    ///
    /// ```text
    /// (EventTarget) -> Node -> CharacterData -> Text | Comment
    ///                       -> Element -> HTMLElement -> HTML<Tag>Element
    ///                                  -> SVGElement
    ///                       -> DocumentFragment
    /// ```
    ///
    /// `Node.prototype` is the object that carries the methods shared by every
    /// wrapper. `EventTarget.prototype` exists as a prototype only; the script
    /// level `EventTarget` constructor is attached to it by the prelude.
    /// Returns `Node.prototype` and the prototype of every interface by name.
    #[allow(
        clippy::too_many_lines,
        reason = "one flat table of interface wiring reads better than fragments"
    )]
    fn install_dom_interfaces(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> (ObjectId, BTreeMap<&'static str, ObjectId>) {
        let mut prototypes: BTreeMap<&'static str, ObjectId> = BTreeMap::new();
        let event_target = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(event_target),
            ..JsObject::default()
        });
        for (name, function) in [
            ("setAttribute", NativeFunction::SetAttribute),
            ("getAttribute", NativeFunction::GetAttribute),
            ("hasAttribute", NativeFunction::HasAttribute),
            ("removeAttribute", NativeFunction::RemoveAttribute),
            ("appendChild", NativeFunction::AppendChild),
            ("removeChild", NativeFunction::RemoveChild),
            ("insertBefore", NativeFunction::InsertBefore),
            ("contains", NativeFunction::Contains),
            ("matches", NativeFunction::Matches),
            ("querySelector", NativeFunction::QuerySelector),
            ("querySelectorAll", NativeFunction::QuerySelectorAll),
            ("addEventListener", NativeFunction::AddEventListener),
            ("removeEventListener", NativeFunction::RemoveEventListener),
            ("dispatchEvent", NativeFunction::DispatchEvent),
            (
                "getBoundingClientRect",
                NativeFunction::GetBoundingClientRect,
            ),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        for name in ["scrollLeft", "scrollTop"] {
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Number(0.0)),
            );
        }
        let node = Self::dom_interface(
            objects,
            global,
            function_prototype,
            "Node",
            prototype,
            None,
            ObjectHost::DomConstructor,
        );
        for (name, value) in [
            ("ELEMENT_NODE", 1.0),
            ("ATTRIBUTE_NODE", 2.0),
            ("TEXT_NODE", 3.0),
            ("CDATA_SECTION_NODE", 4.0),
            ("ENTITY_REFERENCE_NODE", 5.0),
            ("ENTITY_NODE", 6.0),
            ("PROCESSING_INSTRUCTION_NODE", 7.0),
            ("COMMENT_NODE", 8.0),
            ("DOCUMENT_NODE", 9.0),
            ("DOCUMENT_TYPE_NODE", 10.0),
            ("DOCUMENT_FRAGMENT_NODE", 11.0),
            ("NOTATION_NODE", 12.0),
            ("DOCUMENT_POSITION_DISCONNECTED", 1.0),
            ("DOCUMENT_POSITION_PRECEDING", 2.0),
            ("DOCUMENT_POSITION_FOLLOWING", 4.0),
            ("DOCUMENT_POSITION_CONTAINS", 8.0),
            ("DOCUMENT_POSITION_CONTAINED_BY", 16.0),
            ("DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC", 32.0),
        ] {
            for target in [node, prototype] {
                objects[target.0].properties.insert(
                    name.to_owned(),
                    PropertyDescriptor {
                        getter: None,
                        setter: None,
                        value: JsValue::Number(value),
                        writable: false,
                        enumerable: true,
                        configurable: false,
                    },
                );
            }
        }
        prototypes.insert("Node", prototype);

        let mut derive = |name: &'static str,
                          parent: &'static str,
                          host: ObjectHost,
                          objects: &mut Vec<JsObject>|
         -> ObjectId {
            let parent_prototype = prototypes[parent];
            let proto = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(parent_prototype),
                ..JsObject::default()
            });
            let parent_constructor = objects[parent_prototype.0]
                .properties
                .get("constructor")
                .and_then(|descriptor| match descriptor.value {
                    JsValue::Object(object) => Some(object),
                    _ => None,
                });
            Self::dom_interface(
                objects,
                global,
                function_prototype,
                name,
                proto,
                parent_constructor,
                host,
            );
            prototypes.insert(name, proto);
            proto
        };
        derive("CharacterData", "Node", ObjectHost::DomConstructor, objects);
        derive(
            "Text",
            "CharacterData",
            ObjectHost::DomNodeConstructor(DomNodeKind::Text),
            objects,
        );
        derive(
            "Comment",
            "CharacterData",
            ObjectHost::DomNodeConstructor(DomNodeKind::Comment),
            objects,
        );
        derive(
            "DocumentFragment",
            "Node",
            ObjectHost::DomNodeConstructor(DomNodeKind::Fragment),
            objects,
        );
        derive("Element", "Node", ObjectHost::DomConstructor, objects);
        derive(
            "HTMLElement",
            "Element",
            ObjectHost::DomConstructor,
            objects,
        );
        derive("SVGElement", "Element", ObjectHost::DomConstructor, objects);
        derive(
            "SVGSVGElement",
            "SVGElement",
            ObjectHost::DomConstructor,
            objects,
        );
        derive(
            "HTMLMediaElement",
            "HTMLElement",
            ObjectHost::DomConstructor,
            objects,
        );
        for (interface, _) in HTML_ELEMENT_INTERFACES {
            if *interface == "HTMLMediaElement" {
                continue;
            }
            let parent = if matches!(*interface, "HTMLVideoElement" | "HTMLAudioElement") {
                "HTMLMediaElement"
            } else {
                "HTMLElement"
            };
            derive(interface, parent, ObjectHost::DomConstructor, objects);
        }
        (prototype, prototypes)
    }

    /// Create the interface object (constructor) for `prototype` and register
    /// it as the global `name`.
    fn dom_interface(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        function_prototype: ObjectId,
        name: &str,
        prototype: ObjectId,
        parent_constructor: Option<ObjectId>,
        host: ObjectHost,
    ) -> ObjectId {
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(parent_constructor.unwrap_or(function_prototype)),
            host,
            ..JsObject::default()
        });
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        for (key, value) in [
            ("name", JsValue::String(name.to_owned())),
            ("length", JsValue::Number(0.0)),
        ] {
            objects[constructor.0].properties.insert(
                key.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value,
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
        objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(constructor)),
        );
        let tag = JsSymbol::well_known("@@toStringTag");
        objects[prototype.0].symbols.insert(
            tag.id(),
            (
                tag,
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::String(name.to_owned()),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            ),
        );
        objects[global.0].properties.insert(
            name.to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        constructor
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one table row per collection builtin, kept together for review"
    )]
    fn install_collections(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        for (name, kind) in [
            ("Map", CollectionKind::Map),
            ("WeakMap", CollectionKind::WeakMap),
            ("Set", CollectionKind::Set),
            ("WeakSet", CollectionKind::WeakSet),
        ] {
            let prototype = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(object_prototype),
                ..JsObject::default()
            });
            let methods: &[(&str, NativeFunction)] = if kind.is_map() {
                &[
                    ("get", NativeFunction::CollectionGet),
                    ("set", NativeFunction::CollectionSet),
                    ("has", NativeFunction::CollectionHas),
                    ("delete", NativeFunction::CollectionDelete),
                    ("clear", NativeFunction::CollectionClear),
                    ("forEach", NativeFunction::CollectionForEach),
                    ("keys", NativeFunction::CollectionKeys),
                    ("values", NativeFunction::CollectionValues),
                    ("entries", NativeFunction::CollectionEntries),
                ]
            } else {
                &[
                    ("add", NativeFunction::CollectionAdd),
                    ("has", NativeFunction::CollectionHas),
                    ("delete", NativeFunction::CollectionDelete),
                    ("clear", NativeFunction::CollectionClear),
                    ("forEach", NativeFunction::CollectionForEach),
                    ("keys", NativeFunction::CollectionKeys),
                    ("values", NativeFunction::CollectionValues),
                    ("entries", NativeFunction::CollectionEntries),
                    ("union", NativeFunction::CollectionUnion),
                    ("intersection", NativeFunction::CollectionIntersection),
                    ("difference", NativeFunction::CollectionDifference),
                    (
                        "symmetricDifference",
                        NativeFunction::CollectionSymmetricDifference,
                    ),
                    ("isSubsetOf", NativeFunction::CollectionIsSubsetOf),
                    ("isSupersetOf", NativeFunction::CollectionIsSupersetOf),
                    ("isDisjointFrom", NativeFunction::CollectionIsDisjointFrom),
                ]
            };
            for &(method_name, function) in methods {
                // Weak collections intentionally expose only get/set/add,
                // has, and delete. They are not enumerable and have no size.
                if kind.is_weak()
                    && matches!(
                        function,
                        NativeFunction::CollectionClear
                            | NativeFunction::CollectionForEach
                            | NativeFunction::CollectionKeys
                            | NativeFunction::CollectionValues
                            | NativeFunction::CollectionEntries
                            | NativeFunction::CollectionUnion
                            | NativeFunction::CollectionIntersection
                            | NativeFunction::CollectionDifference
                            | NativeFunction::CollectionSymmetricDifference
                            | NativeFunction::CollectionIsSubsetOf
                            | NativeFunction::CollectionIsSupersetOf
                            | NativeFunction::CollectionIsDisjointFrom
                    )
                {
                    continue;
                }
                let method = ObjectId(objects.len());
                objects.push(JsObject {
                    prototype: Some(function_prototype),
                    host: ObjectHost::NativeFunction(function),
                    ..JsObject::default()
                });
                objects[prototype.0].properties.insert(
                    method_name.to_owned(),
                    PropertyDescriptor::builtin(JsValue::Object(method)),
                );
                // `map[Symbol.iterator]` aliases `entries`; `set[Symbol.iterator]`
                // aliases `values`, exactly as the spec installs them.
                if method_name == (if kind.is_map() { "entries" } else { "values" }) {
                    let symbol = JsSymbol::well_known("@@iterator");
                    objects[prototype.0].symbols.insert(
                        symbol.id(),
                        (symbol, PropertyDescriptor::builtin(JsValue::Object(method))),
                    );
                }
            }
            let constructor = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::CollectionConstructor(kind),
                ..JsObject::default()
            });
            objects[constructor.0].properties.insert(
                "prototype".to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(prototype),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
            objects[prototype.0].properties.insert(
                "constructor".to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(constructor)),
            );
            // ECMA-262 24.1.3.14, 24.2.3.13, 24.3.3.12, 24.4.3.13: the prototype's
            // `@@toStringTag` names the collection, and it is what
            // `Object.prototype.toString` reads.
            let tag = JsSymbol::well_known("@@toStringTag");
            objects[prototype.0].symbols.insert(
                tag.id(),
                (
                    tag,
                    PropertyDescriptor {
                        getter: None,
                        setter: None,
                        value: JsValue::String(name.to_owned()),
                        writable: false,
                        enumerable: false,
                        configurable: true,
                    },
                ),
            );
            objects[global.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(constructor),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
    }

    /// Install the typed-array family (ECMA-262 23.2): `%TypedArray%`, the
    /// abstract constructor whose prototype holds the methods and accessors every
    /// concrete typed array shares, and the concrete constructors (`Int8Array`
    /// through `Float64Array`), which inherit from it.
    #[allow(
        clippy::too_many_lines,
        reason = "bootstrap tables read best as a single listing"
    )]
    fn install_typed_arrays(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        // §23.2.3: `%TypedArray%.prototype`, which the concrete prototypes inherit.
        let intrinsic_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        // §23.2.1: `%TypedArray%`, which is not directly constructable.
        let intrinsic = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::TypedArrayIntrinsic),
            ..JsObject::default()
        });
        objects[intrinsic.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(intrinsic_prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[intrinsic.0].properties.insert(
            "name".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::String("TypedArray".to_owned()),
                writable: false,
                enumerable: false,
                configurable: true,
            },
        );
        Self::install_length(objects, intrinsic, 0.0);
        objects[intrinsic_prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(intrinsic)),
        );
        // §23.2.2.1 and §23.2.2.2: `%TypedArray%.from` and `%TypedArray%.of`.
        for (name, function, arity) in [
            ("from", NativeFunction::TypedArrayFrom, 1.0),
            ("of", NativeFunction::TypedArrayOf, 0.0),
        ] {
            let method = Self::install_native_method(objects, function_prototype, function, arity);
            objects[intrinsic.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        // §23.2.2.4: `get %TypedArray%[@@species]` returns `this`.
        Self::install_species_getter(
            objects,
            function_prototype,
            intrinsic,
            NativeFunction::TypedArraySpecies,
        );
        // §23.2.3: the prototype methods, with their `length`s.
        let methods: &[(&str, NativeFunction, f64)] = &[
            ("at", NativeFunction::TypedArrayAt, 1.0),
            ("copyWithin", NativeFunction::TypedArrayCopyWithin, 2.0),
            ("entries", NativeFunction::TypedArrayEntries, 0.0),
            ("every", NativeFunction::TypedArrayEvery, 1.0),
            ("fill", NativeFunction::TypedArrayFill, 1.0),
            ("filter", NativeFunction::TypedArrayFilter, 1.0),
            ("find", NativeFunction::TypedArrayFind, 1.0),
            ("findIndex", NativeFunction::TypedArrayFindIndex, 1.0),
            ("findLast", NativeFunction::TypedArrayFindLast, 1.0),
            (
                "findLastIndex",
                NativeFunction::TypedArrayFindLastIndex,
                1.0,
            ),
            ("forEach", NativeFunction::TypedArrayForEach, 1.0),
            ("includes", NativeFunction::TypedArrayIncludes, 1.0),
            ("indexOf", NativeFunction::TypedArrayIndexOf, 1.0),
            ("join", NativeFunction::TypedArrayJoin, 1.0),
            ("keys", NativeFunction::TypedArrayKeys, 0.0),
            ("lastIndexOf", NativeFunction::TypedArrayLastIndexOf, 1.0),
            ("map", NativeFunction::TypedArrayMap, 1.0),
            ("reduce", NativeFunction::TypedArrayReduce, 1.0),
            ("reduceRight", NativeFunction::TypedArrayReduceRight, 1.0),
            ("reverse", NativeFunction::TypedArrayReverse, 0.0),
            ("set", NativeFunction::TypedArraySet, 1.0),
            ("slice", NativeFunction::TypedArraySlice, 2.0),
            ("some", NativeFunction::TypedArraySome, 1.0),
            ("sort", NativeFunction::TypedArraySort, 1.0),
            ("subarray", NativeFunction::TypedArraySubarray, 2.0),
            ("toReversed", NativeFunction::TypedArrayToReversed, 0.0),
            ("toSorted", NativeFunction::TypedArrayToSorted, 1.0),
            ("toString", NativeFunction::TypedArrayJoin, 0.0),
            ("values", NativeFunction::TypedArrayValues, 0.0),
            ("with", NativeFunction::TypedArrayWith, 2.0),
        ];
        let mut values_method = None;
        for &(name, function, arity) in methods {
            let method = Self::install_native_method(objects, function_prototype, function, arity);
            objects[intrinsic_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
            if name == "values" {
                values_method = Some(method);
            }
        }
        // §23.2.3.33: `%TypedArray%.prototype[@@iterator]` is the `values` function.
        if let Some(values) = values_method {
            let symbol = JsSymbol::well_known("@@iterator");
            objects[intrinsic_prototype.0].symbols.insert(
                symbol.id(),
                (symbol, PropertyDescriptor::builtin(JsValue::Object(values))),
            );
        }
        // §23.2.3.3, §23.2.3.19 and §23.2.3.23: the `length`, `byteLength` and
        // `byteOffset` accessors.
        Self::install_getters(
            objects,
            function_prototype,
            intrinsic_prototype,
            &[
                ("buffer", NativeFunction::TypedArrayBufferGetter),
                ("length", NativeFunction::TypedArrayLengthGetter),
                ("byteLength", NativeFunction::TypedArrayByteLengthGetter),
                ("byteOffset", NativeFunction::TypedArrayByteOffsetGetter),
            ],
        );
        // §23.2.3.32: `@@toStringTag` is an accessor that names the element kind.
        let tag_getter = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::TypedArrayToStringTagGetter),
            ..JsObject::default()
        });
        let tag = JsSymbol::well_known("@@toStringTag");
        objects[intrinsic_prototype.0].symbols.insert(
            tag.id(),
            (
                tag,
                PropertyDescriptor {
                    value: JsValue::Undefined,
                    writable: false,
                    getter: Some(tag_getter),
                    setter: None,
                    enumerable: false,
                    configurable: true,
                },
            ),
        );
        for kind in TypedArrayKind::ALL {
            let prototype = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(intrinsic_prototype),
                ..JsObject::default()
            });
            #[allow(
                clippy::cast_precision_loss,
                reason = "element sizes are tiny integers"
            )]
            let bytes_per_element = JsValue::Number(kind.element_size() as f64);
            objects[prototype.0].properties.insert(
                "BYTES_PER_ELEMENT".to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: bytes_per_element.clone(),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
            let constructor = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(intrinsic),
                host: ObjectHost::TypedArrayConstructor(kind),
                ..JsObject::default()
            });
            objects[constructor.0].properties.insert(
                "prototype".to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(prototype),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
            objects[constructor.0].properties.insert(
                "BYTES_PER_ELEMENT".to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: bytes_per_element,
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
            // §23.2.5.1: every concrete constructor declares three parameters.
            Self::install_length(objects, constructor, 3.0);
            objects[prototype.0].properties.insert(
                "constructor".to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(constructor)),
            );
            objects[global.0].properties.insert(
                kind.name().to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(constructor),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
    }

    /// A built-in method object with an explicit `length`.
    fn install_native_method(
        objects: &mut Vec<JsObject>,
        function_prototype: ObjectId,
        function: NativeFunction,
        arity: f64,
    ) -> ObjectId {
        let method = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(function),
            ..JsObject::default()
        });
        Self::install_length(objects, method, arity);
        method
    }

    /// Install an interface whose instances carry an `ObjectHost` state, with
    /// the given prototype methods and a `Symbol.toStringTag` naming the host.
    /// Shared by `DataView` and `TextDecoder`.
    #[allow(clippy::too_many_arguments)]
    fn install_host_interface(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
        name: &'static str,
        constructor_host: ObjectHost,
        methods: &[(&str, NativeFunction)],
        tag: &str,
    ) -> (ObjectId, ObjectId) {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (method_name, native) in methods {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(*native),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                (*method_name).to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        // §20.1.3.6 step 7: a string-valued `Symbol.toStringTag` names the host,
        // so a polyfill can branch on it.
        let symbol = JsSymbol::well_known("@@toStringTag");
        objects[prototype.0].symbols.insert(
            symbol.id(),
            (
                symbol,
                PropertyDescriptor::builtin(JsValue::String(tag.to_owned())),
            ),
        );
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: constructor_host,
            ..JsObject::default()
        });
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(prototype)),
        );
        objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(constructor)),
        );
        objects[global.0].properties.insert(
            name.to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        (prototype, constructor)
    }

    /// Install accessor properties with getter-only functions on `target`.
    fn install_getters(
        objects: &mut Vec<JsObject>,
        function_prototype: ObjectId,
        target: ObjectId,
        getters: &[(&str, NativeFunction)],
    ) {
        for &(name, getter) in getters {
            let accessor = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(getter),
                properties: Self::function_metadata(&format!("get {name}"), 0.0),
                ..JsObject::default()
            });
            objects[target.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    value: JsValue::Undefined,
                    writable: false,
                    getter: Some(accessor),
                    setter: None,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
    }

    /// `TextEncoder`, `TextDecoder` (Encoding Standard) and `DataView`
    /// (ECMAScript 25.2.5).
    ///
    /// `TextEncoder` is UTF-8 only, so its instances carry no state. `TextDecoder`
    /// keeps its label, its `fatal` flag, and the bytes a streaming decode held
    /// back; `DataView` keeps the shared buffer and its byte window. All three
    /// are installed here so a page that feature-detects them finds all or the
    /// one it asked for, never a half-present interface.
    #[allow(clippy::too_many_lines)]
    fn install_encoding(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        // `ArrayBuffer` comes first because it is the buffer every binary
        // format constructs a `DataView` over, and a `DataView` a script cannot
        // build is a global that exists and cannot be used.
        let (array_buffer_prototype, array_buffer_constructor) = Self::install_host_interface(
            objects,
            global,
            object_prototype,
            function_prototype,
            "ArrayBuffer",
            ObjectHost::ArrayBufferConstructor,
            &[("slice", NativeFunction::ArrayBufferSlice)],
            "ArrayBuffer",
        );
        Self::install_getters(
            objects,
            function_prototype,
            array_buffer_prototype,
            &[
                ("byteLength", NativeFunction::ArrayBufferByteLengthGetter),
                (
                    "maxByteLength",
                    NativeFunction::ArrayBufferMaxByteLengthGetter,
                ),
                ("resizable", NativeFunction::ArrayBufferResizableGetter),
                ("detached", NativeFunction::ArrayBufferDetachedGetter),
            ],
        );
        for (name, function, arity) in [
            ("resize", NativeFunction::ArrayBufferResize, 1.0),
            ("transfer", NativeFunction::ArrayBufferTransfer, 0.0),
            (
                "transferToFixedLength",
                NativeFunction::ArrayBufferTransferToFixedLength,
                0.0,
            ),
        ] {
            let method = Self::install_native_method(objects, function_prototype, function, arity);
            objects[array_buffer_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        // §25.1.4.1 and §25.2.2.1: `ArrayBuffer(length)` and
        // `DataView(buffer [, byteOffset [, byteLength]])` both have a `length`
        // of 1.
        Self::install_length(objects, array_buffer_constructor, 1.0);
        // §25.1.5.3: `get ArrayBuffer[@@species]` returns `this`.
        Self::install_species_getter(
            objects,
            function_prototype,
            array_buffer_constructor,
            NativeFunction::ArrayBufferSpecies,
        );
        // §25.2: `SharedArrayBuffer`, the same host with its own brand, growable
        // through `grow`, and its own species getter.
        let (shared_prototype, shared_constructor) = Self::install_host_interface(
            objects,
            global,
            object_prototype,
            function_prototype,
            "SharedArrayBuffer",
            ObjectHost::SharedArrayBufferConstructor,
            &[],
            "SharedArrayBuffer",
        );
        Self::install_getters(
            objects,
            function_prototype,
            shared_prototype,
            &[
                (
                    "byteLength",
                    NativeFunction::SharedArrayBufferByteLengthGetter,
                ),
                ("growable", NativeFunction::SharedArrayBufferGrowableGetter),
                (
                    "maxByteLength",
                    NativeFunction::SharedArrayBufferMaxByteLengthGetter,
                ),
            ],
        );
        for (name, function, arity) in [
            ("grow", NativeFunction::SharedArrayBufferGrow, 1.0),
            ("slice", NativeFunction::SharedArrayBufferSlice, 2.0),
        ] {
            let method = Self::install_native_method(objects, function_prototype, function, arity);
            objects[shared_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        Self::install_length(objects, shared_constructor, 1.0);
        Self::install_species_getter(
            objects,
            function_prototype,
            shared_constructor,
            NativeFunction::ArrayBufferSpecies,
        );
        // §25.1.5.1: `ArrayBuffer.isView` is a static, bound to the constructor
        // the way `Array.isArray` is.
        let is_view = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::BoundFunction {
                function: NativeFunction::ArrayBufferIsView,
                receiver: array_buffer_constructor,
            },
            properties: Self::function_metadata("isView", 1.0),
            ..JsObject::default()
        });
        objects[array_buffer_constructor.0].properties.insert(
            "isView".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(is_view)),
        );

        let encoder_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (method, native) in [
            ("encode", NativeFunction::TextEncoderEncode),
            ("encodeInto", NativeFunction::TextEncoderEncodeInto),
        ] {
            let function = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(native),
                ..JsObject::default()
            });
            objects[encoder_prototype.0].properties.insert(
                method.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(function)),
            );
        }
        // `TextEncoder.prototype.encoding` is always "utf-8": the constructor
        // takes no arguments, and a UTF-8-only encoder is the whole interface.
        objects[encoder_prototype.0].properties.insert(
            "encoding".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::String("utf-8".to_owned()),
                writable: false,
                enumerable: true,
                configurable: true,
            },
        );
        let encoder = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::TextEncoderConstructor,
            ..JsObject::default()
        });
        objects[encoder.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(encoder_prototype)),
        );
        objects[encoder_prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(encoder)),
        );
        objects[global.0].properties.insert(
            "TextEncoder".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(encoder),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );

        // `TextDecoder.prototype.encoding` and `.fatal` are read-only accessor
        // properties in the spec whose answers depend on the label the
        // constructor was given, so the prototype carries no value for them: the
        // constructor installs an own non-writable data property per instance.
        // A prototype default would be a lie for every non-default label.
        let decoder_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let decode = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::TextDecoderDecode),
            ..JsObject::default()
        });
        objects[decoder_prototype.0].properties.insert(
            "decode".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(decode)),
        );
        let decoder = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::TextDecoderConstructor,
            ..JsObject::default()
        });
        objects[decoder.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(decoder_prototype)),
        );
        objects[decoder_prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(decoder)),
        );
        objects[global.0].properties.insert(
            "TextDecoder".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(decoder),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );

        let (data_view_prototype, data_view_constructor) = Self::install_host_interface(
            objects,
            global,
            object_prototype,
            function_prototype,
            "DataView",
            ObjectHost::DataViewConstructor,
            &[
                ("getInt8", NativeFunction::DataViewGetInt8),
                ("getUint8", NativeFunction::DataViewGetUint8),
                ("getInt16", NativeFunction::DataViewGetInt16),
                ("getUint16", NativeFunction::DataViewGetUint16),
                ("getInt32", NativeFunction::DataViewGetInt32),
                ("getUint32", NativeFunction::DataViewGetUint32),
                ("getFloat32", NativeFunction::DataViewGetFloat32),
                ("getFloat16", NativeFunction::DataViewGetFloat16),
                ("getFloat64", NativeFunction::DataViewGetFloat64),
                ("getBigInt64", NativeFunction::DataViewGetBigInt64),
                ("getBigUint64", NativeFunction::DataViewGetBigUint64),
                ("setInt8", NativeFunction::DataViewSetInt8),
                ("setUint8", NativeFunction::DataViewSetUint8),
                ("setInt16", NativeFunction::DataViewSetInt16),
                ("setUint16", NativeFunction::DataViewSetUint16),
                ("setInt32", NativeFunction::DataViewSetInt32),
                ("setUint32", NativeFunction::DataViewSetUint32),
                ("setFloat16", NativeFunction::DataViewSetFloat16),
                ("setFloat32", NativeFunction::DataViewSetFloat32),
                ("setFloat64", NativeFunction::DataViewSetFloat64),
                ("setBigInt64", NativeFunction::DataViewSetBigInt64),
                ("setBigUint64", NativeFunction::DataViewSetBigUint64),
            ],
            "DataView",
        );
        Self::install_getters(
            objects,
            function_prototype,
            data_view_prototype,
            &[
                ("buffer", NativeFunction::DataViewBufferGetter),
                ("byteLength", NativeFunction::DataViewByteLengthGetter),
                ("byteOffset", NativeFunction::DataViewByteOffsetGetter),
            ],
        );
        Self::install_length(objects, data_view_constructor, 1.0);
    }

    /// Give `constructor` a `get [Symbol.species]` accessor whose getter is
    /// `getter`, a native that returns `this` (non-enumerable, configurable).
    fn install_species_getter(
        objects: &mut Vec<JsObject>,
        function_prototype: ObjectId,
        constructor: ObjectId,
        getter: NativeFunction,
    ) {
        let accessor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(getter),
            properties: Self::function_metadata("get [Symbol.species]", 0.0),
            ..JsObject::default()
        });
        let species = JsSymbol::well_known("@@species");
        objects[constructor.0].symbols.insert(
            species.id(),
            (
                species,
                PropertyDescriptor {
                    getter: Some(accessor),
                    setter: None,
                    value: JsValue::Undefined,
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            ),
        );
    }

    /// Give a built-in constructor or function its `length` own property
    /// (non-writable, non-enumerable, configurable).
    fn install_length(objects: &mut [JsObject], function: ObjectId, length: f64) {
        objects[function.0].properties.insert(
            "length".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Number(length),
                writable: false,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// `%AsyncIteratorPrototype%` with `[Symbol.asyncIterator]`, and its
    /// descendants `%AsyncGeneratorPrototype%` (ECMA-262 27.6.1) and
    /// `%AsyncFromSyncIteratorPrototype%` (27.1.4). Returns those two.
    fn install_async_iteration(
        objects: &mut Vec<JsObject>,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> (ObjectId, ObjectId, ObjectId) {
        let async_iterator = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let self_iterator = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::IteratorPrototypeIterator),
            ..JsObject::default()
        });
        let symbol = JsSymbol::well_known("@@asyncIterator");
        objects[async_iterator.0].symbols.insert(
            symbol.id(),
            (
                symbol.clone(),
                PropertyDescriptor::builtin(JsValue::Object(self_iterator)),
            ),
        );

        let generator = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(async_iterator),
            ..JsObject::default()
        });
        Self::install_methods(
            objects,
            generator,
            function_prototype,
            [
                ("next", NativeFunction::AsyncGeneratorNext),
                ("return", NativeFunction::AsyncGeneratorReturn),
                ("throw", NativeFunction::AsyncGeneratorThrow),
            ],
        );
        Self::set_one_parameter_lengths(objects, generator, &["next", "return", "throw"]);
        let tag = JsSymbol::well_known("@@toStringTag");
        objects[generator.0].symbols.insert(
            tag.id(),
            (
                tag,
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::String("AsyncGenerator".to_owned()),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            ),
        );

        let from_sync = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(async_iterator),
            ..JsObject::default()
        });
        Self::install_methods(
            objects,
            from_sync,
            function_prototype,
            [
                ("next", NativeFunction::AsyncFromSyncNext),
                ("return", NativeFunction::AsyncFromSyncReturn),
                ("throw", NativeFunction::AsyncFromSyncThrow),
            ],
        );
        Self::set_one_parameter_lengths(objects, from_sync, &["next", "return", "throw"]);

        // %AsyncGeneratorFunction.prototype% (ECMA-262 27.7.1) and its links.
        let generator_function = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            ..JsObject::default()
        });
        let link = |value: JsValue, writable: bool| PropertyDescriptor {
            getter: None,
            setter: None,
            value,
            writable,
            enumerable: false,
            configurable: true,
        };
        objects[generator_function.0].properties.insert(
            "prototype".to_owned(),
            link(JsValue::Object(generator), false),
        );
        objects[generator.0].properties.insert(
            "constructor".to_owned(),
            link(JsValue::Object(generator_function), false),
        );
        let tag = JsSymbol::well_known("@@toStringTag");
        objects[generator_function.0].symbols.insert(
            tag.id(),
            (
                tag,
                link(JsValue::String("AsyncGeneratorFunction".to_owned()), false),
            ),
        );
        (generator, from_sync, generator_function)
    }

    /// Give the methods of `target` named in `names` the spec `length` of one.
    /// [`Self::builtin_arity`] goes by name alone, so it cannot tell a
    /// generator's `next` (one parameter) from an iterator helper's (none).
    fn set_one_parameter_lengths(objects: &mut [JsObject], target: ObjectId, names: &[&str]) {
        for name in names {
            let Some(JsValue::Object(method)) = objects[target.0]
                .properties
                .get(*name)
                .map(|descriptor| descriptor.value.clone())
            else {
                continue;
            };
            objects[method.0].properties.insert(
                "length".to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Number(1.0),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
    }

    /// Give `target` one builtin method object per `(name, function)` pair.
    fn install_methods<const N: usize>(
        objects: &mut Vec<JsObject>,
        target: ObjectId,
        function_prototype: ObjectId,
        methods: [(&str, NativeFunction); N],
    ) {
        for (name, function) in methods {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[target.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
    }

    /// `%GeneratorPrototype%` (ECMA-262 §27.5.1): inherits the iterator
    /// helpers and `[Symbol.iterator]` from `%IteratorPrototype%`.
    fn install_generator(
        objects: &mut Vec<JsObject>,
        function_prototype: ObjectId,
        iterator_prototype: ObjectId,
    ) -> ObjectId {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(iterator_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("next", NativeFunction::GeneratorNext),
            ("return", NativeFunction::GeneratorReturn),
            ("throw", NativeFunction::GeneratorThrow),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        Self::set_one_parameter_lengths(objects, prototype, &["next", "return", "throw"]);
        let tag = JsSymbol::well_known("@@toStringTag");
        objects[prototype.0].symbols.insert(
            tag.id(),
            (
                tag,
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::String("Generator".to_owned()),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            ),
        );
        prototype
    }

    /// Installs the `console` object with the standard logging methods.
    ///
    /// Messages are buffered in the runtime and drained by the embedding; the
    /// interpreter never touches I/O itself.
    /// Install the `Iterator` global, `%IteratorPrototype%` (helper methods),
    /// and `%IteratorHelperPrototype%`. Returns both prototypes.
    #[allow(clippy::too_many_lines)]
    fn install_iterator(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> (ObjectId, ObjectId, ObjectId) {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let helper_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(prototype),
            ..JsObject::default()
        });
        // Each method carries its spec `length`, set here because the name-based
        // arity table cannot tell `Iterator.concat` (0) from `Array.prototype.concat` (1).
        for (name, function, arity) in [
            ("map", NativeFunction::IteratorMap, 1.0),
            ("filter", NativeFunction::IteratorFilter, 1.0),
            ("take", NativeFunction::IteratorTake, 1.0),
            ("drop", NativeFunction::IteratorDrop, 1.0),
            ("flatMap", NativeFunction::IteratorFlatMap, 1.0),
            ("reduce", NativeFunction::IteratorReduce, 1.0),
            ("toArray", NativeFunction::IteratorToArray, 0.0),
            ("forEach", NativeFunction::IteratorForEach, 1.0),
            ("some", NativeFunction::IteratorSome, 1.0),
            ("every", NativeFunction::IteratorEvery, 1.0),
            ("find", NativeFunction::IteratorFind, 1.0),
            ("includes", NativeFunction::IteratorIncludes, 1.0),
            ("join", NativeFunction::IteratorJoin, 1.0),
            ("chunks", NativeFunction::IteratorChunks, 1.0),
            ("windows", NativeFunction::IteratorWindows, 1.0),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[method.0].properties.insert(
                "length".to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Number(arity),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            );
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        let self_iterator = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::IteratorPrototypeIterator),
            ..JsObject::default()
        });
        let symbol = JsSymbol::well_known("@@iterator");
        objects[prototype.0].symbols.insert(
            symbol.id(),
            (
                symbol.clone(),
                PropertyDescriptor::builtin(JsValue::Object(self_iterator)),
            ),
        );
        // `%IteratorHelperPrototype%` and `%WrapForValidIteratorPrototype%` each
        // carry their own `next`/`return` pair (ECMA-262 27.1.2.1, 27.1.3.2).
        let wrap_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(prototype),
            ..JsObject::default()
        });
        for (target, methods) in [
            (
                helper_prototype,
                [
                    ("next", NativeFunction::IteratorHelperNext),
                    ("return", NativeFunction::IteratorHelperReturn),
                ],
            ),
            (
                wrap_prototype,
                [
                    ("next", NativeFunction::IteratorWrapNext),
                    ("return", NativeFunction::IteratorWrapReturn),
                ],
            ),
        ] {
            for (name, function) in methods {
                let method = ObjectId(objects.len());
                objects.push(JsObject {
                    prototype: Some(function_prototype),
                    host: ObjectHost::NativeFunction(function),
                    ..JsObject::default()
                });
                objects[target.0].properties.insert(
                    name.to_owned(),
                    PropertyDescriptor::builtin(JsValue::Object(method)),
                );
            }
        }
        objects[helper_prototype.0].symbols.insert(
            symbol.id(),
            (
                symbol,
                PropertyDescriptor::builtin(JsValue::Object(self_iterator)),
            ),
        );
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::IteratorConstructor),
            ..JsObject::default()
        });
        for (name, function, arity) in [
            ("from", NativeFunction::IteratorFrom, 1.0),
            ("concat", NativeFunction::IteratorConcat, 0.0),
            ("zip", NativeFunction::IteratorZip, 1.0),
            ("zipKeyed", NativeFunction::IteratorZipKeyed, 1.0),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[method.0].properties.insert(
                "length".to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Number(arity),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            );
            objects[constructor.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[constructor.0].properties.insert(
            "length".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Number(0.0),
                writable: false,
                enumerable: false,
                configurable: true,
            },
        );
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[global.0].properties.insert(
            "Iterator".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        (prototype, helper_prototype, wrap_prototype)
    }

    /// Attach spec-visible `name`/`length` own properties to built-in
    /// functions and `X.prototype.constructor` back-references that the
    /// individual install loops omit.
    #[allow(
        clippy::too_many_lines,
        reason = "builtin metadata is a single bootstrap pass"
    )]
    fn install_builtin_metadata(objects: &mut [JsObject]) {
        let mut names: Vec<Option<String>> = vec![None; objects.len()];
        let mut prototype_owner: Vec<Option<ObjectId>> = vec![None; objects.len()];
        let mut callable: Vec<bool> = vec![false; objects.len()];
        for (index, object) in objects.iter().enumerate() {
            // One answer, from one list. This used to be a second hand-written
            // copy of the callable-host set, and it had drifted from the first:
            // `ArrayBuffer`, `DataView`, `TextEncoder` and `TextDecoder` were
            // missing from *both* copies at the same time, so `typeof
            // ArrayBuffer` answered `"object"` and the constructor had no `name`
            // and no `length`. Two lists cannot be kept in agreement by review;
            // one [`Self::is_callable_host`] can, because a new constructor
            // variant now fails to compile in exactly one place.
            callable[index] = object.host.is_callable();
            for (key, descriptor) in &object.properties {
                let JsValue::Object(target) = &descriptor.value else {
                    continue;
                };
                if names[target.0].is_none() {
                    names[target.0] = Some(key.clone());
                }
                if key == "prototype" && prototype_owner[target.0].is_none() {
                    prototype_owner[target.0] = Some(ObjectId(index));
                }
            }
            for (symbol, descriptor) in object.symbols.values() {
                let JsValue::Object(target) = &descriptor.value else {
                    continue;
                };
                if names[target.0].is_none()
                    && let Some(description) = symbol.description()
                {
                    // A well-known symbol is named by its specification name, which the
                    // engine stores as `@@iterator` and reports as `[Symbol.iterator]`.
                    names[target.0] = Some(match description.strip_prefix("@@") {
                        Some(well_known) => format!("[Symbol.{well_known}]"),
                        None => format!("[{description}]"),
                    });
                }
            }
        }
        for (index, object) in objects.iter_mut().enumerate() {
            if let Some(owner) = prototype_owner[index]
                && callable[owner.0]
                && !object.properties.contains_key("constructor")
            {
                object.properties.insert(
                    "constructor".to_owned(),
                    PropertyDescriptor {
                        getter: None,
                        setter: None,
                        value: JsValue::Object(owner),
                        writable: true,
                        enumerable: false,
                        configurable: true,
                    },
                );
            }
            if !callable[index] {
                continue;
            }
            let name = names[index].clone().unwrap_or_default();
            if !object.properties.contains_key("name") {
                object.properties.insert(
                    "name".to_owned(),
                    PropertyDescriptor {
                        getter: None,
                        setter: None,
                        value: JsValue::String(name.clone()),
                        writable: false,
                        enumerable: false,
                        configurable: true,
                    },
                );
            }
            if !object.properties.contains_key("length") {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "builtin arities are tiny integers"
                )]
                let length = Self::builtin_arity(&name) as f64;
                object.properties.insert(
                    "length".to_owned(),
                    PropertyDescriptor {
                        getter: None,
                        setter: None,
                        value: JsValue::Number(length),
                        writable: false,
                        enumerable: false,
                        configurable: true,
                    },
                );
            }
        }
    }

    /// Spec arity for the common built-in names; unknown names fall back to
    /// zero. A wrong arity only affects `length` assertions.
    ///
    /// **Every name here is a name the engine installs.** That is a checked
    /// invariant rather than a hope: the arity-table test in
    /// `runtime::tests` walks the realm and fails if a name in this table is not
    /// a callable the engine put there. The table used to carry forty names
    /// nothing installed - `String.prototype.codePointAt`, `Promise.all`,
    /// `Array.prototype.flat`, `Object.is`, and thirty-six more - which had no
    /// runtime effect at all (an absent name is never looked up) and a large
    /// documentation effect: it read as a capability list, so `codePointAt` looked
    /// implemented to anyone scanning the source, and an agent briefed that "the
    /// platform surface is mostly there" would believe it. A list of things that
    /// do not exist is worse than no list, so the list is now only the things
    /// that do.
    ///
    /// Adding a member means adding it here, and the test is what makes that
    /// necessary rather than optional: a method implemented and not listed gets a
    /// `length` of `0`, which is wrong but harmless, and a method listed and not
    /// implemented is a lie, which is not.
    #[allow(
        clippy::match_same_arms,
        clippy::too_many_lines,
        reason = "the arity groups read better split by feature area"
    )]
    fn builtin_arity(name: &str) -> usize {
        match name {
            "push"
            | "map"
            | "filter"
            | "forEach"
            | "some"
            | "every"
            | "find"
            | "findIndex"
            | "findLast"
            | "findLastIndex"
            | "includes"
            | "indexOf"
            | "lastIndexOf"
            | "charAt"
            | "charCodeAt"
            | "codePointAt"
            | "at"
            | "repeat"
            | "resolve"
            | "reject"
            | "catch"
            | "finally"
            | "get"
            | "has"
            | "add"
            | "union"
            | "intersection"
            | "difference"
            | "symmetricDifference"
            | "isSubsetOf"
            | "isSupersetOf"
            | "isDisjointFrom"
            | "bind"
            | "isArray"
            | "from"
            | "getOwnPropertyDescriptors"
            | "getOwnPropertySymbols"
            | "getPrototypeOf"
            | "hasOwnProperty"
            | "isPrototypeOf"
            | "propertyIsEnumerable"
            | "parseFloat"
            | "isNaN"
            | "isFinite"
            | "parse"
            | "exec"
            | "test"
            | "toFixed"
            | "toPrecision"
            | "match"
            | "matchAll"
            | "search"
            | "localeCompare"
            | "startsWith"
            | "endsWith"
            | "sort"
            | "reduce"
            | "reduceRight"
            | "fill"
            | "flatMap"
            | "freeze"
            | "seal"
            | "preventExtensions"
            | "isFrozen"
            | "isSealed"
            | "isExtensible"
            | "getOwnPropertyNames"
            // Math (ECMA-262 21.3.2): one argument.
            | "abs" | "acos" | "acosh" | "asin" | "asinh" | "atan" | "atanh" | "cbrt"
            | "ceil" | "clz32" | "cos" | "cosh" | "exp" | "expm1" | "floor" | "fround"
            | "log" | "log1p" | "log10" | "log2" | "round" | "sign" | "sin" | "sinh"
            | "sqrt" | "tan" | "tanh" | "trunc" => 1,
            // DataView accessors (ECMA-262 25.2.4): a getter takes the request
            // index, and a setter takes the index and the value.
            "getInt8" | "getUint8" | "getInt16" | "getUint16" | "getInt32" | "getUint32"
            | "getFloat16" | "getFloat32" | "getFloat64" | "getBigInt64" | "getBigUint64" => 1,
            "setInt8" | "setUint8" | "setInt16" | "setUint16" | "setInt32" | "setUint32"
            | "setFloat16" | "setFloat32" | "setFloat64" | "setBigInt64" | "setBigUint64" => 2,
            // Array, String and Object members whose `length` is one (ECMA-262
            // 23.1.3.1, 22.1.3.1, 20.1.2.x and B.2.2.x).
            "concat" | "unshift" | "join" | "fromEntries" | "__lookupGetter__"
            | "__lookupSetter__" | "toSorted" | "fromAsync" | "padStart" | "padEnd"
            | "fromCharCode" | "fromCodePoint" | "raw" | "anchor" | "fontcolor"
            | "fontsize" | "link" => 1,
            // Math (ECMA-262 21.3.2): two arguments.
            "atan2" | "hypot" | "imul" | "max" | "min" | "pow" => 2,
            "then"
            | "set"
            | "apply"
            | "create"
            | "defineProperties"
            | "replace"
            | "replaceAll"
            | "split"
            | "slice"
            | "substring"
            | "substr"
            | "splice"
            | "parseInt"
            | "assign"
            | "getOwnPropertyDescriptor"
            | "setPrototypeOf"
            | "copyWithin"
            | "toSpliced"
            | "with"
            | "hasOwn"
            | "__defineGetter__"
            | "__defineSetter__"
            | "groupBy"
            | "is" => 2,
            "defineProperty" => 3,
            // Date setters (ECMA-262 21.4.4.20-28, B.2.3.2) and `toJSON` (21.4.4.37).
            "setDate" | "setMilliseconds" | "setTime" | "setUTCDate" | "setUTCMilliseconds"
            | "setYear" | "toJSON" => 1,
            "setMonth" | "setSeconds" | "setUTCMonth" | "setUTCSeconds" => 2,
            "setFullYear" | "setMinutes" | "setUTCFullYear" | "setUTCMinutes" => 3,
            "setHours" | "setUTCHours" => 4,
            "construct" => 2,
            "toString" | "valueOf" | "toISOString" | "toUTCString" | "toDateString" | "now"
            | "getTime" | "getFullYear" | "getUTCFullYear" | "getMonth" | "getUTCMonth"
            | "getDate" | "getUTCDate" | "getDay" | "getUTCDay" | "getHours" | "getUTCHours"
            | "getMinutes" | "getUTCMinutes" | "getSeconds" | "getUTCSeconds"
            | "getMilliseconds" | "getUTCMilliseconds" | "getTimezoneOffset" | "pop" | "shift"
            | "clear" | "next" | "return" | "random" | "flat" | "keys" | "values" | "entries"
            | "toArray" | "toLowerCase" | "toUpperCase" => 0,
            "Object" | "Function" | "Array" | "String" | "Number" | "Boolean" | "Error"
            | "TypeError" | "RangeError" | "SyntaxError" | "ReferenceError" | "EvalError"
            | "URIError" | "Promise" | "ArrayBuffer" | "DataView" | "Symbol" | "Map" | "Set"
            | "WeakMap" | "WeakSet" | "Iterator" | "Uint8Array" | "Uint8ClampedArray"
            | "Int8Array" | "Uint16Array" | "Int16Array" | "Uint32Array" | "Int32Array"
            | "Float32Array" | "Float64Array" => 1,
            "BigInt" => 1,
            "Date" | "UTC" => 7,
            "RegExp" | "asIntN" | "asUintN" => 2,
            _ => 0,
        }
    }

    fn install_console(objects: &mut Vec<JsObject>, global: ObjectId) {
        let console = ObjectId(objects.len());
        objects.push(JsObject::default());
        for (name, function) in [
            ("debug", NativeFunction::ConsoleDebug),
            ("error", NativeFunction::ConsoleError),
            ("info", NativeFunction::ConsoleInfo),
            ("log", NativeFunction::ConsoleLog),
            ("warn", NativeFunction::ConsoleWarn),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::BoundFunction {
                    function,
                    receiver: console,
                },
                ..JsObject::default()
            });
            objects[console.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(method),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
        objects[global.0].properties.insert(
            "console".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(console),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// Installs the global timer functions (`setTimeout`, `setInterval`, and
    /// the animation-frame pair).
    ///
    /// The runtime only records callback identities and requested delays;
    /// actual scheduling belongs to the embedding, which drains pending
    /// timer requests after each script execution.
    fn install_timers(objects: &mut Vec<JsObject>, global: ObjectId) {
        for (name, function) in [
            ("setTimeout", NativeFunction::SetTimeout),
            ("setInterval", NativeFunction::SetInterval),
            ("clearTimeout", NativeFunction::ClearTimeout),
            ("clearInterval", NativeFunction::ClearInterval),
            (
                "requestAnimationFrame",
                NativeFunction::RequestAnimationFrame,
            ),
            ("cancelAnimationFrame", NativeFunction::CancelAnimationFrame),
            ("addEventListener", NativeFunction::WindowAddEventListener),
            (
                "removeEventListener",
                NativeFunction::WindowRemoveEventListener,
            ),
        ] {
            Self::define_global_function(objects, global, name, function);
        }
    }

    /// Install one Web Storage area with the `Storage` method set. The area's
    /// entries are its own properties, so the ordinary own-key machinery gives
    /// `key(n)` insertion order and `Object.keys` enumeration for free.
    fn install_storage(
        objects: &mut Vec<JsObject>,
        _global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> ObjectId {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (method, function) in [
            ("getItem", NativeFunction::StorageGetItem),
            ("setItem", NativeFunction::StorageSetItem),
            ("removeItem", NativeFunction::StorageRemoveItem),
            ("clear", NativeFunction::StorageClear),
            ("key", NativeFunction::StorageKey),
        ] {
            let function_object = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                method.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(function_object)),
            );
        }
        let tag = JsSymbol::well_known("@@toStringTag");
        objects[prototype.0].symbols.insert(
            tag.id(),
            (
                tag,
                PropertyDescriptor::builtin(JsValue::String("Storage".to_owned())),
            ),
        );
        prototype
    }

    /// Install one Web Storage area. Its entries are its own properties, so
    /// `length` is just the own-key count and `key(n)` follows the shared
    /// insertion order of the ordinary property table.
    fn install_storage_area(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        storage_prototype: ObjectId,
        name: &str,
    ) {
        let storage = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(storage_prototype),
            host: ObjectHost::Storage,
            ..JsObject::default()
        });
        objects[global.0].properties.insert(
            name.to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(storage),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
    }

    fn install_location(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        document: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
        url: &Url,
    ) -> ObjectId {
        let location = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            host: ObjectHost::Location(url.clone()),
            ..JsObject::default()
        });
        let to_string = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::LocationToString),
            ..JsObject::default()
        });
        objects[location.0].properties.insert(
            "toString".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(to_string)),
        );
        for (name, function) in [
            ("assign", NativeFunction::LocationAssign),
            ("replace", NativeFunction::LocationReplace),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::BoundFunction {
                    function,
                    receiver: location,
                },
                ..JsObject::default()
            });
            objects[location.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        for (name, value) in location_components(url) {
            objects[location.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::String(value)),
            );
        }
        for owner in [global, document] {
            objects[owner.0].properties.insert(
                "location".to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(location),
                    writable: false,
                    enumerable: true,
                    configurable: false,
                },
            );
        }
        // The top-level browsing context is its own parent and top window.
        for name in ["window", "self", "globalThis", "parent", "top"] {
            objects[global.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(global),
                    writable: false,
                    enumerable: true,
                    configurable: false,
                },
            );
        }
        location
    }

    fn install_navigator(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
    ) {
        let navigator = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, value) in [
            ("userAgent", "Mozilla/5.0 rENDER/0.1"),
            ("appName", "Netscape"),
            ("appVersion", "5.0 (rENDER)"),
            ("platform", "Win32"),
            ("language", "zh-CN"),
        ] {
            objects[navigator.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::String(value.to_owned())),
            );
        }
        for (name, value) in [("cookieEnabled", true), ("onLine", true)] {
            objects[navigator.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Boolean(value)),
            );
        }
        objects[global.0].properties.insert(
            "navigator".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(navigator),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
    }

    /// Install the read-only screen metrics used by responsive site
    /// bootstrap code.  The renderer updates `innerWidth`/`innerHeight` from
    /// the live viewport; screen dimensions are a stable desktop baseline in
    /// this single-window embedding and remain useful for feature detection.
    fn install_screen(objects: &mut Vec<JsObject>, global: ObjectId, object_prototype: ObjectId) {
        let screen = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, value) in [
            ("width", 1_024.0),
            ("height", 768.0),
            ("availWidth", 1_024.0),
            ("availHeight", 768.0),
            ("colorDepth", 24.0),
            ("pixelDepth", 24.0),
        ] {
            objects[screen.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Number(value)),
            );
        }
        objects[global.0].properties.insert(
            "screen".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(screen),
                writable: false,
                enumerable: false,
                configurable: true,
            },
        );
    }

    fn install_performance(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        let performance = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let now = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::BoundFunction {
                function: NativeFunction::PerformanceNow,
                receiver: performance,
            },
            ..JsObject::default()
        });
        objects[performance.0].properties.insert(
            "now".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(now)),
        );
        objects[performance.0].properties.insert(
            "timeOrigin".to_owned(),
            PropertyDescriptor::builtin(JsValue::Number(0.0)),
        );
        let timing = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        objects[timing.0].properties.insert(
            "navigationStart".to_owned(),
            PropertyDescriptor::builtin(JsValue::Number(0.0)),
        );
        objects[performance.0].properties.insert(
            "timing".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(timing)),
        );
        for (name, function) in [
            ("getEntries", NativeFunction::PerformanceGetEntries),
            (
                "getEntriesByType",
                NativeFunction::PerformanceGetEntriesByType,
            ),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[performance.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[global.0].properties.insert(
            "performance".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(performance),
                writable: false,
                enumerable: false,
                configurable: true,
            },
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "bootstrap tables read best as a single listing"
    )]
    fn install_object(objects: &mut Vec<JsObject>, global: ObjectId) -> ObjectId {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject::default());
        for (name, function) in [
            (
                "hasOwnProperty",
                NativeFunction::ObjectPrototypeHasOwnProperty,
            ),
            (
                "isPrototypeOf",
                NativeFunction::ObjectPrototypeIsPrototypeOf,
            ),
            (
                "propertyIsEnumerable",
                NativeFunction::ObjectPrototypePropertyIsEnumerable,
            ),
            ("toString", NativeFunction::ObjectPrototypeToString),
            (
                "toLocaleString",
                NativeFunction::ObjectPrototypeToLocaleString,
            ),
            ("__defineGetter__", NativeFunction::ObjectDefineGetter),
            ("__defineSetter__", NativeFunction::ObjectDefineSetter),
            ("__lookupGetter__", NativeFunction::ObjectLookupGetter),
            ("__lookupSetter__", NativeFunction::ObjectLookupSetter),
            ("valueOf", NativeFunction::ObjectPrototypeValueOf),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(method),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
        let object = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::ObjectConstructor,
            ..JsObject::default()
        });
        // Annex B.2.2.1 `Object.prototype.__proto__`: an accessor pair, not a
        // data property. Frameworks and polyfills read and write it directly
        // (`node.__proto__[SYMBOL] = value`, `{ __proto__: base }`), and no
        // engine leaves it undefined.
        let mut proto_accessor = [None, None];
        for (slot, (function, name, length)) in proto_accessor.iter_mut().zip([
            (NativeFunction::ObjectProtoGetter, "get __proto__", 0.0),
            (NativeFunction::ObjectProtoSetter, "set __proto__", 1.0),
        ]) {
            let accessor = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            // ECMA-262 10.2.9 SetFunctionName gives accessors a `get `/`set ` prefix.
            for (key, value) in [
                ("length", JsValue::Number(length)),
                ("name", JsValue::String(name.to_owned())),
            ] {
                objects[accessor.0].properties.insert(
                    key.to_owned(),
                    PropertyDescriptor {
                        getter: None,
                        setter: None,
                        value,
                        writable: false,
                        enumerable: false,
                        configurable: true,
                    },
                );
            }
            *slot = Some(accessor);
        }
        objects[prototype.0].properties.insert(
            "__proto__".to_owned(),
            PropertyDescriptor {
                value: JsValue::Undefined,
                writable: false,
                getter: proto_accessor[0],
                setter: proto_accessor[1],
                enumerable: false,
                configurable: true,
            },
        );
        for (name, function) in [
            ("assign", NativeFunction::ObjectAssign),
            ("keys", NativeFunction::ObjectKeys),
            ("values", NativeFunction::ObjectValues),
            ("entries", NativeFunction::ObjectEntries),
            ("create", NativeFunction::ObjectCreate),
            ("defineProperty", NativeFunction::ObjectDefineProperty),
            ("defineProperties", NativeFunction::ObjectDefineProperties),
            (
                "getOwnPropertyDescriptor",
                NativeFunction::ObjectGetOwnPropertyDescriptor,
            ),
            (
                "getOwnPropertyDescriptors",
                NativeFunction::ObjectGetOwnPropertyDescriptors,
            ),
            (
                "getOwnPropertyNames",
                NativeFunction::ObjectGetOwnPropertyNames,
            ),
            (
                "getOwnPropertySymbols",
                NativeFunction::ObjectGetOwnPropertySymbols,
            ),
            ("getPrototypeOf", NativeFunction::ObjectGetPrototypeOf),
            ("setPrototypeOf", NativeFunction::ObjectSetPrototypeOf),
            ("hasOwn", NativeFunction::ObjectHasOwn),
            ("preventExtensions", NativeFunction::ObjectPreventExtensions),
            ("seal", NativeFunction::ObjectSeal),
            ("freeze", NativeFunction::ObjectFreeze),
            ("isExtensible", NativeFunction::ObjectIsExtensible),
            ("isSealed", NativeFunction::ObjectIsSealed),
            ("isFrozen", NativeFunction::ObjectIsFrozen),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::BoundFunction {
                    function,
                    receiver: object,
                },
                ..JsObject::default()
            });
            objects[object.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[object.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[global.0].properties.insert(
            "Object".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(object),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        Self::define_prototype_constructor(objects, prototype, object);
        prototype
    }

    /// ECMA-262 `String.prototype[Symbol.iterator]`, the only iterator String has
    /// (there is no `values` alias). Real bundles need it
    /// for `get-intrinsic`, which reads
    /// `getProto(getProto("x"[Symbol.iterator]()))` to capture
    /// `%IteratorPrototype%`; without it the lenient missing-host-call path
    /// answers `undefined` and the whole intrinsic table is lost.
    /// ECMA-262 Annex B.2.2 HTML methods on `String.prototype`: one native per
    /// row of [`HTML_METHODS`], named by its row.
    fn install_string_html(
        objects: &mut Vec<JsObject>,
        prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        for (index, &(name, _, _)) in HTML_METHODS.iter().enumerate() {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(NativeFunction::StrHtml(index)),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
    }

    fn install_string_iterator(
        objects: &mut Vec<JsObject>,
        prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        let iterator = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::StrIterator),
            ..JsObject::default()
        });
        let symbol = JsSymbol::well_known("@@iterator");
        objects[prototype.0].symbols.insert(
            symbol.id(),
            (
                symbol,
                PropertyDescriptor::builtin(JsValue::Object(iterator)),
            ),
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "String.prototype is one table of method names, one row per method"
    )]
    fn install_string(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> ObjectId {
        let string = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::StringConstructor,
            ..JsObject::default()
        });
        objects[global.0].properties.insert(
            "String".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(string),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        for (name, function) in [
            ("fromCharCode", NativeFunction::StringFromCharCode),
            ("fromCodePoint", NativeFunction::StringFromCodePoint),
            ("raw", NativeFunction::StringRaw),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[string.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("charAt", NativeFunction::StrCharAt),
            ("charCodeAt", NativeFunction::StrCharCodeAt),
            ("codePointAt", NativeFunction::StrCodePointAt),
            ("at", NativeFunction::StrAt),
            ("indexOf", NativeFunction::StrIndexOf),
            ("lastIndexOf", NativeFunction::StrLastIndexOf),
            ("includes", NativeFunction::StrIncludes),
            ("startsWith", NativeFunction::StrStartsWith),
            ("endsWith", NativeFunction::StrEndsWith),
            ("slice", NativeFunction::StrSlice),
            ("substring", NativeFunction::StrSubstring),
            ("substr", NativeFunction::StringSubstr),
            ("padStart", NativeFunction::StrPadStart),
            ("padEnd", NativeFunction::StrPadEnd),
            ("toLowerCase", NativeFunction::StrToLowerCase),
            ("toUpperCase", NativeFunction::StrToUpperCase),
            ("toLocaleLowerCase", NativeFunction::StrToLowerCase),
            ("toLocaleUpperCase", NativeFunction::StrToUpperCase),
            ("trim", NativeFunction::StrTrim),
            ("trimStart", NativeFunction::StrTrimStart),
            ("trimEnd", NativeFunction::StrTrimEnd),
            ("repeat", NativeFunction::StrRepeat),
            ("localeCompare", NativeFunction::StrLocaleCompare),
            ("split", NativeFunction::StrSplit),
            ("replace", NativeFunction::StrReplace),
            ("replaceAll", NativeFunction::StrReplaceAll),
            ("match", NativeFunction::StrMatch),
            ("matchAll", NativeFunction::StrMatchAll),
            ("search", NativeFunction::StrSearch),
            ("concat", NativeFunction::StrConcat),
            ("isWellFormed", NativeFunction::StrIsWellFormed),
            ("toWellFormed", NativeFunction::StrToWellFormed),
            ("toString", NativeFunction::StrToString),
            ("valueOf", NativeFunction::StrToString),
            ("forEach", NativeFunction::StrForEach),
            ("push", NativeFunction::StrPush),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        Self::install_string_iterator(objects, prototype, function_prototype);
        Self::install_string_html(objects, prototype, function_prototype);
        objects[string.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        Self::define_prototype_constructor(objects, prototype, string);
        prototype
    }

    /// `X.prototype.constructor === X` for builtin constructors.
    fn define_prototype_constructor(
        objects: &mut [JsObject],
        prototype: ObjectId,
        constructor: ObjectId,
    ) {
        objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// The `name` and `length` own properties of a built-in function whose
    /// arity the name table cannot give (a symbol-keyed or a species method).
    pub(crate) fn function_metadata(
        name: &str,
        length: f64,
    ) -> BTreeMap<String, PropertyDescriptor> {
        let read_only = |value: JsValue| PropertyDescriptor {
            getter: None,
            setter: None,
            value,
            writable: false,
            enumerable: false,
            configurable: true,
        };
        BTreeMap::from([
            ("length".to_owned(), read_only(JsValue::Number(length))),
            (
                "name".to_owned(),
                read_only(JsValue::String(name.to_owned())),
            ),
        ])
    }

    /// `%RegExpStringIteratorPrototype%` (ECMA-262 22.2.9): its `next`, and the
    /// `RegExp String Iterator` tag, over `%IteratorPrototype%`.
    fn install_regexp_string_iterator(
        objects: &mut Vec<JsObject>,
        iterator_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> ObjectId {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(iterator_prototype),
            ..JsObject::default()
        });
        let next = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::RegExpStringIteratorNext),
            properties: Self::function_metadata("next", 0.0),
            ..JsObject::default()
        });
        objects[prototype.0].properties.insert(
            "next".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(next)),
        );
        let tag = JsSymbol::well_known("@@toStringTag");
        objects[prototype.0].symbols.insert(
            tag.id(),
            (
                tag,
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::String("RegExp String Iterator".to_owned()),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            ),
        );
        prototype
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the RegExp intrinsics are installed together, in one place"
    )]
    fn install_regexp(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> ObjectId {
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::RegExpConstructor,
            ..JsObject::default()
        });
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("exec", NativeFunction::RegExpExec),
            ("test", NativeFunction::RegExpTest),
            ("toString", NativeFunction::RegExpToString),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        // ECMA-262 22.2.6: the source and flag properties are accessors on the
        // prototype, computed from each instance's compiled record.
        for (name, accessor) in [
            ("source", RegExpAccessor::Source),
            ("flags", RegExpAccessor::Flags),
            ("global", RegExpAccessor::Global),
            ("ignoreCase", RegExpAccessor::IgnoreCase),
            ("multiline", RegExpAccessor::Multiline),
            ("dotAll", RegExpAccessor::DotAll),
            ("sticky", RegExpAccessor::Sticky),
            ("unicode", RegExpAccessor::Unicode),
            ("unicodeSets", RegExpAccessor::UnicodeSets),
            ("hasIndices", RegExpAccessor::HasIndices),
        ] {
            let getter = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(NativeFunction::RegExpAccessor(accessor)),
                properties: Self::function_metadata(&format!("get {name}"), 0.0),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
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
        // ECMA-262 22.2.6.8-12: the Symbol protocol methods are keyed by the
        // well-known symbols, and carry their `[Symbol.x]` names and arities.
        for (symbol, function, arity) in [
            ("@@match", NativeFunction::RegExpSymbolMatch, 1.0),
            ("@@matchAll", NativeFunction::RegExpSymbolMatchAll, 1.0),
            ("@@replace", NativeFunction::RegExpSymbolReplace, 2.0),
            ("@@search", NativeFunction::RegExpSymbolSearch, 1.0),
            ("@@split", NativeFunction::RegExpSymbolSplit, 2.0),
        ] {
            let method = ObjectId(objects.len());
            let key = JsSymbol::well_known(symbol);
            let display = format!("[Symbol.{}]", &symbol[2..]);
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                properties: Self::function_metadata(&display, arity),
                ..JsObject::default()
            });
            objects[prototype.0].symbols.insert(
                key.id(),
                (key, PropertyDescriptor::builtin(JsValue::Object(method))),
            );
        }
        let escape = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::RegExpEscape),
            properties: Self::function_metadata("escape", 1.0),
            ..JsObject::default()
        });
        objects[constructor.0].properties.insert(
            "escape".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(escape)),
        );
        // ECMA-262 22.2.5.2: `get RegExp[@@species]` returns `this`.
        let species_getter = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::RegExpSpecies),
            properties: Self::function_metadata("get [Symbol.species]", 0.0),
            ..JsObject::default()
        });
        let species = JsSymbol::well_known("@@species");
        objects[constructor.0].symbols.insert(
            species.id(),
            (
                species,
                PropertyDescriptor {
                    getter: Some(species_getter),
                    setter: None,
                    value: JsValue::Undefined,
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            ),
        );
        objects[prototype.0].properties.insert(
            "lastIndex".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Number(0.0),
                writable: true,
                enumerable: false,
                configurable: false,
            },
        );
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(constructor)),
        );
        objects[global.0].properties.insert(
            "RegExp".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        prototype
    }

    /// Install the `Number` constructor with its well-known constants.
    fn install_number(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> ObjectId {
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NumberConstructor,
            ..JsObject::default()
        });
        for (name, value) in [
            ("MAX_SAFE_INTEGER", 9_007_199_254_740_991.0),
            ("MIN_SAFE_INTEGER", -9_007_199_254_740_991.0),
            ("EPSILON", f64::EPSILON),
            ("MAX_VALUE", f64::MAX),
            ("MIN_VALUE", f64::MIN_POSITIVE),
            ("POSITIVE_INFINITY", f64::INFINITY),
            ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
            ("NaN", f64::NAN),
        ] {
            objects[constructor.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Number(value),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
        }
        for (name, function) in [
            ("isInteger", NumberOp::IsInteger),
            ("isFinite", NumberOp::IsFinite),
            ("isNaN", NumberOp::IsNaN),
            ("isSafeInteger", NumberOp::IsSafeInteger),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(NativeFunction::NumberOp(function)),
                ..JsObject::default()
            });
            objects[constructor.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[global.0].properties.insert(
            "Number".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        // Number primitive wrapper prototype.
        let num_proto = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("toFixed", NativeFunction::NumToFixed),
            ("toPrecision", NativeFunction::NumToPrecision),
            ("toString", NativeFunction::NumToString),
            ("valueOf", NativeFunction::NumValueOf),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[num_proto.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[num_proto.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(constructor)),
        );
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(num_proto),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        num_proto
    }

    /// Install the `Boolean` constructor.
    fn install_boolean(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> ObjectId {
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::BooleanConstructor,
            ..JsObject::default()
        });
        objects[global.0].properties.insert(
            "Boolean".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        // Boolean primitive wrapper prototype.
        let bool_proto = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("toString", NativeFunction::BoolToString),
            ("valueOf", NativeFunction::BoolValueOf),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[bool_proto.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[bool_proto.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(constructor)),
        );
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(bool_proto),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        bool_proto
    }

    /// Install the `Date` constructor, prototype, and `Date.now`.
    #[allow(clippy::too_many_lines)]
    fn install_date(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> ObjectId {
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::DateConstructor,
            ..JsObject::default()
        });
        let now = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::NativeFunction(NativeFunction::DateNow),
            ..JsObject::default()
        });
        objects[constructor.0].properties.insert(
            "now".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(now)),
        );
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("getTime", NativeFunction::DateGetValue),
            ("setTime", NativeFunction::DateSetTime),
            ("setDate", NativeFunction::DateSetDate),
            ("setFullYear", NativeFunction::DateSetFullYear),
            ("setHours", NativeFunction::DateSetHours),
            ("setMilliseconds", NativeFunction::DateSetMilliseconds),
            ("setMinutes", NativeFunction::DateSetMinutes),
            ("setMonth", NativeFunction::DateSetMonth),
            ("setSeconds", NativeFunction::DateSetSeconds),
            ("setUTCDate", NativeFunction::DateSetUTCDate),
            ("setUTCFullYear", NativeFunction::DateSetUTCFullYear),
            ("setUTCHours", NativeFunction::DateSetUTCHours),
            ("setUTCMilliseconds", NativeFunction::DateSetUTCMilliseconds),
            ("setUTCMinutes", NativeFunction::DateSetUTCMinutes),
            ("setUTCMonth", NativeFunction::DateSetUTCMonth),
            ("setUTCSeconds", NativeFunction::DateSetUTCSeconds),
            ("setYear", NativeFunction::DateSetYear),
            ("getYear", NativeFunction::DateGetYear),
            ("getFullYear", NativeFunction::DateGetFullYear),
            ("getMonth", NativeFunction::DateGetMonth),
            ("getDate", NativeFunction::DateGetDate),
            ("getDay", NativeFunction::DateGetDay),
            ("getHours", NativeFunction::DateGetHours),
            ("getMinutes", NativeFunction::DateGetMinutes),
            ("getSeconds", NativeFunction::DateGetSeconds),
            ("getMilliseconds", NativeFunction::DateGetMilliseconds),
            ("getTimezoneOffset", NativeFunction::DateGetTimezoneOffset),
            ("getUTCFullYear", NativeFunction::DateGetUTCFullYear),
            ("getUTCMonth", NativeFunction::DateGetUTCMonth),
            ("getUTCDate", NativeFunction::DateGetUTCDate),
            ("getUTCDay", NativeFunction::DateGetUTCDay),
            ("getUTCHours", NativeFunction::DateGetUTCHours),
            ("getUTCMinutes", NativeFunction::DateGetUTCMinutes),
            ("getUTCSeconds", NativeFunction::DateGetUTCSeconds),
            ("getUTCMilliseconds", NativeFunction::DateGetUTCMilliseconds),
            ("valueOf", NativeFunction::DateValueOf),
            ("toString", NativeFunction::DateToString),
            ("toLocaleString", NativeFunction::DateToLocaleString),
            ("toLocaleDateString", NativeFunction::DateToLocaleDateString),
            ("toLocaleTimeString", NativeFunction::DateToLocaleTimeString),
            ("toTimeString", NativeFunction::DateToTimeString),
            ("toUTCString", NativeFunction::DateToGMTString),
            ("toDateString", NativeFunction::DateToDateString),
            ("toISOString", NativeFunction::DateToISOString),
            ("toJSON", NativeFunction::DateToJSON),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        // ECMA-262 21.4.4.45: `[Symbol.toPrimitive]` is read-only and configurable,
        // with length one.
        let to_primitive = JsSymbol::well_known("@@toPrimitive");
        let to_primitive_method = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::NativeFunction(NativeFunction::DateSymbolToPrimitive),
            ..JsObject::default()
        });
        Self::install_length(objects, to_primitive_method, 1.0);
        objects[prototype.0].symbols.insert(
            to_primitive.id(),
            (
                to_primitive,
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(to_primitive_method),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            ),
        );
        // Annex B.2.3.3: `toGMTString` is the same function object as `toUTCString`.
        // Its name is set explicitly, because the metadata pass would otherwise take
        // it from whichever of the two keys sorts first.
        if let Some(JsValue::Object(utc_string)) = objects[prototype.0]
            .properties
            .get("toUTCString")
            .map(|descriptor| descriptor.value.clone())
        {
            objects[utc_string.0].properties.insert(
                "name".to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::String("toUTCString".to_owned()),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            );
            objects[prototype.0].properties.insert(
                "toGMTString".to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(utc_string)),
            );
        }
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        for (name, function) in [
            ("parse", NativeFunction::DateParse),
            ("UTC", NativeFunction::DateUTC),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[constructor.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[global.0].properties.insert(
            "Date".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        prototype
    }

    /// Install the `Symbol` constructor (value-level subset: unique tokens).
    fn install_symbol(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> ObjectId {
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::SymbolConstructor,
            ..JsObject::default()
        });
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("toString", NativeFunction::SymbolToString),
            ("valueOf", NativeFunction::SymbolValueOf),
            ("[description]", NativeFunction::SymbolDescription),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        // `Symbol.prototype.description` is a getter-only accessor (spec);
        // reuse the method object installed above as the getter.
        if let Some(descriptor) = objects[prototype.0].properties.remove("[description]")
            && let JsValue::Object(getter) = descriptor.value
        {
            objects[prototype.0].properties.insert(
                "description".to_owned(),
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
        // Well-known symbols are real symbol values at fixed ids so engine
        // internals can key on them without a registry lookup.
        for (name, key) in [
            ("iterator", "@@iterator"),
            ("asyncIterator", "@@asyncIterator"),
            ("toStringTag", "@@toStringTag"),
            ("toPrimitive", "@@toPrimitive"),
            ("hasInstance", "@@hasInstance"),
            ("species", "@@species"),
            ("isConcatSpreadable", "@@isConcatSpreadable"),
            ("unscopables", "@@unscopables"),
            ("match", "@@match"),
            ("matchAll", "@@matchAll"),
            ("replace", "@@replace"),
            ("search", "@@search"),
            ("split", "@@split"),
        ] {
            objects[constructor.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Symbol(JsSymbol::well_known(key))),
            );
        }
        Self::install_dispose_symbols(objects, constructor);
        // `Symbol.prototype[Symbol.toStringTag] === "Symbol"`
        {
            let tag = JsSymbol::well_known("@@toStringTag");
            objects[prototype.0].symbols.insert(
                tag.id(),
                (
                    tag,
                    PropertyDescriptor::builtin(JsValue::String("Symbol".to_owned())),
                ),
            );
        }
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[global.0].properties.insert(
            "Symbol".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        prototype
    }

    /// Install the `BigInt` function and `BigInt.prototype` (ECMA-262 21.2).
    /// `BigInt` is callable but not a constructor: its host is absent from the
    /// constructor list, so `new BigInt()` takes the usual `TypeError` path.
    fn install_bigint(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> ObjectId {
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::BigIntConstructor,
            ..JsObject::default()
        });
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("asIntN", NativeFunction::BigIntAsIntN),
            ("asUintN", NativeFunction::BigIntAsUintN),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[constructor.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        for (name, function) in [
            ("toString", NativeFunction::BigIntToString),
            ("valueOf", NativeFunction::BigIntValueOf),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(constructor)),
        );
        // `BigInt.prototype[Symbol.toStringTag] === "BigInt"`
        {
            let tag = JsSymbol::well_known("@@toStringTag");
            objects[prototype.0].symbols.insert(
                tag.id(),
                (
                    tag,
                    // Unlike the usual built-in property, the tag is read-only.
                    PropertyDescriptor {
                        getter: None,
                        setter: None,
                        value: JsValue::String("BigInt".to_owned()),
                        writable: false,
                        enumerable: false,
                        configurable: true,
                    },
                ),
            );
        }
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[global.0].properties.insert(
            "BigInt".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        prototype
    }

    /// A fresh wrapper object hosting a `BigInt` primitive, so its methods are
    /// reachable through `BigInt.prototype` (`Object(1n)`, `(1n).toString()`).
    pub(crate) fn bigint_primitive_wrapper(&mut self, value: JsBigInt) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.bigint_prototype),
            host: ObjectHost::BigIntPrimitive(value),
            ..JsObject::default()
        })
    }

    /// A fresh wrapper object hosting a symbol primitive, used when a
    /// symbol's methods are accessed (`sym.toString()`).
    pub(crate) fn symbol_instance_wrapper(&mut self, symbol: JsSymbol) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.symbol_prototype),
            host: ObjectHost::SymbolInstance(symbol),
            ..JsObject::default()
        })
    }

    fn install_event(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let prevent_default = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::EventPreventDefault),
            ..JsObject::default()
        });
        objects[prototype.0].properties.insert(
            "preventDefault".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(prevent_default)),
        );
        for (name, function) in [
            ("stopPropagation", NativeFunction::EventStopPropagation),
            (
                "stopImmediatePropagation",
                NativeFunction::EventStopImmediatePropagation,
            ),
            ("composedPath", NativeFunction::EventComposedPath),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        for (name, value) in [
            ("NONE", 0.0),
            ("CAPTURING_PHASE", 1.0),
            ("AT_TARGET", 2.0),
            ("BUBBLING_PHASE", 3.0),
        ] {
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Number(value),
                    writable: false,
                    enumerable: true,
                    configurable: false,
                },
            );
        }

        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::EventConstructor,
            ..JsObject::default()
        });
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[global.0].properties.insert(
            "Event".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// The ECMAScript error hierarchy, returning the `%Error.prototype%` the
    /// derived constructors hang off. `DOMException` installs against that
    /// object rather than against `Object.prototype`, which is the whole of
    /// `WebIDL` §3.14.1's JavaScript binding for it.
    fn install_errors(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> ObjectId {
        let error_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let to_string = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::ErrorPrototypeToString),
            ..JsObject::default()
        });
        objects[error_prototype.0].properties.insert(
            "toString".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(to_string)),
        );

        // §20.5.6.2: every NativeError constructor inherits from `Error`, so its
        // [[Prototype]] is the `Error` constructor, which `ErrorKind::ALL` lists first.
        let mut error_constructor = function_prototype;
        for kind in ErrorKind::ALL {
            let prototype = if kind == ErrorKind::Error {
                error_prototype
            } else {
                let prototype = ObjectId(objects.len());
                objects.push(JsObject {
                    prototype: Some(error_prototype),
                    ..JsObject::default()
                });
                prototype
            };
            let constructor = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(error_constructor),
                host: ObjectHost::ErrorConstructor(kind),
                ..JsObject::default()
            });
            if kind == ErrorKind::Error {
                error_constructor = constructor;
            }
            objects[constructor.0].properties.insert(
                "prototype".to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(prototype),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
            objects[constructor.0].properties.insert(
                "name".to_owned(),
                PropertyDescriptor::builtin(JsValue::String(kind.name().to_owned())),
            );
            objects[constructor.0].properties.insert(
                "length".to_owned(),
                PropertyDescriptor::builtin(JsValue::Number(1.0)),
            );
            for (name, value) in [("name", kind.name()), ("message", "")] {
                objects[prototype.0].properties.insert(
                    name.to_owned(),
                    PropertyDescriptor::builtin(JsValue::String(value.to_owned())),
                );
            }
            objects[prototype.0].properties.insert(
                "constructor".to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(constructor)),
            );
            objects[global.0].properties.insert(
                kind.name().to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(constructor),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
        error_prototype
    }

    /// `DOMException` (`WebIDL` §4.4) and the `Error` prototype it hangs off.
    ///
    /// The heritage is the part worth being explicit about, because the IDL and
    /// the JavaScript binding disagree on purpose. The IDL fragment declares
    /// `interface DOMException` with **no** inheritance clause, so in the
    /// specification's own type system a `DOMException` is not an `Error` and
    /// nothing about `Error`'s members is inherited by it. But `WebIDL`
    /// §3.14.1 overrides that for the JavaScript binding: "the interface
    /// prototype object for `DOMException` has its [[Prototype]] internal slot set
    /// to the intrinsic object %Error.prototype%" and "It also has [[`ErrorData`]]
    /// and [[Stack]] slots, like all built-in exceptions."
    ///
    /// The binding wins at runtime, and it wins deliberately: `instanceof Error`
    /// is the check real code writes when it wants to know whether a rejection or
    /// a thrown value is an exception at all, and a `DOMException` that reported
    /// `false` there would be a worse answer than the heritage difference it
    /// papers over. So the prototype chain is `DOMException.prototype ->
    /// Error.prototype -> Object.prototype`, and `String(e)` is
    /// `Error.prototype.toString`'s `"name: message"`, both of which real code
    /// relies on. What the binding does *not* do is make `DOMException` a
    /// subclass in the IDL sense: there is no `DOMException` in
    /// `TypeError.prototype`'s chain, and no error constructor derives from
    /// `DOMException`.
    fn install_dom_exception(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        error_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(error_prototype),
            ..JsObject::default()
        });
        // WebIDL §2.7.2: an interface prototype object's @@toStringTag is the
        // interface name, and ECMA-262 `Object.prototype.toString` step 20 reads
        // it, so this is what makes
        // `Object.prototype.toString.call(new DOMException())` answer
        // `"[object DOMException]"` rather than the `[[ErrorData]]` fallback
        // `"Error"`. Both answers are real - the object is an error and it is a
        // DOMException - and the tag is the one the binding specifies.
        let to_string_tag = JsSymbol::well_known("@@toStringTag");
        objects[prototype.0].symbols.insert(
            to_string_tag.id(),
            (
                to_string_tag,
                PropertyDescriptor::builtin(JsValue::String("DOMException".to_owned())),
            ),
        );
        // WebIDL §2.5.2: a `readonly attribute` is an accessor on the prototype
        // backed by an internal slot, so these are accessors and not own data
        // properties. `Object.getOwnPropertyNames(new DOMException())` is
        // therefore empty apart from `stack`, and a write to `e.name` is refused
        // rather than quietly shadowing the slot.
        for (name, getter) in [
            ("name", NativeFunction::DomExceptionNameGetter),
            ("message", NativeFunction::DomExceptionMessageGetter),
            ("code", NativeFunction::DomExceptionCodeGetter),
        ] {
            let accessor = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(getter),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    value: JsValue::Undefined,
                    writable: false,
                    getter: Some(accessor),
                    setter: None,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::DomExceptionConstructor,
            ..JsObject::default()
        });
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(constructor)),
        );
        // WebIDL §2.5.1: a constant is readable through the interface object and
        // through instances, is enumerable, and is neither writable nor
        // configurable.
        for (constant, code) in DOM_EXCEPTION_LEGACY_CODES {
            let value = PropertyDescriptor {
                value: JsValue::Number(f64::from(code)),
                writable: false,
                getter: None,
                setter: None,
                enumerable: true,
                configurable: false,
            };
            objects[constructor.0]
                .properties
                .insert(constant.to_owned(), value.clone());
            objects[prototype.0]
                .properties
                .insert(constant.to_owned(), value);
        }
        objects[global.0].properties.insert(
            "DOMException".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// The `name` or `length` slot of a built-in function: non-writable,
    /// non-enumerable and configurable (ECMA-262 10.2.9 and 10.2.10).
    fn function_metadata_slot(value: JsValue) -> PropertyDescriptor {
        PropertyDescriptor {
            value,
            writable: false,
            getter: None,
            setter: None,
            enumerable: false,
            configurable: true,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Function and its prototype methods install together"
    )]
    fn install_function(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
    ) -> ObjectId {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::FunctionPrototype),
            ..JsObject::default()
        });
        // ECMA-262 20.2.3: `Function.prototype` is itself a function named "" of length 0.
        objects[prototype.0].properties.insert(
            "length".to_owned(),
            Self::function_metadata_slot(JsValue::Number(0.0)),
        );
        objects[prototype.0].properties.insert(
            "name".to_owned(),
            Self::function_metadata_slot(JsValue::String(String::new())),
        );
        for (name, function, length) in [
            ("toString", NativeFunction::FunctionToString, 0.0),
            ("call", NativeFunction::FunctionCall, 1.0),
            ("bind", NativeFunction::FunctionBind, 1.0),
            ("apply", NativeFunction::FunctionApply, 2.0),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[method.0].properties.insert(
                "length".to_owned(),
                Self::function_metadata_slot(JsValue::Number(length)),
            );
            objects[method.0].properties.insert(
                "name".to_owned(),
                Self::function_metadata_slot(JsValue::String(name.to_owned())),
            );
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(method),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
        // ECMA-262 20.2.3.6 `Function.prototype[@@hasInstance]`: non-writable,
        // non-enumerable and non-configurable, named "[Symbol.hasInstance]".
        let has_instance = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(prototype),
            host: ObjectHost::NativeFunction(NativeFunction::FunctionHasInstance),
            ..JsObject::default()
        });
        objects[has_instance.0].properties.insert(
            "length".to_owned(),
            Self::function_metadata_slot(JsValue::Number(1.0)),
        );
        objects[has_instance.0].properties.insert(
            "name".to_owned(),
            Self::function_metadata_slot(JsValue::String("[Symbol.hasInstance]".to_owned())),
        );
        let symbol = JsSymbol::well_known("@@hasInstance");
        objects[prototype.0].symbols.insert(
            symbol.id(),
            (
                symbol,
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(has_instance),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            ),
        );
        let function = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(prototype),
            host: ObjectHost::FunctionConstructor,
            ..JsObject::default()
        });
        objects[function.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[global.0].properties.insert(
            "Function".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(function),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        prototype
    }

    fn install_math(objects: &mut Vec<JsObject>, global: ObjectId, object_prototype: ObjectId) {
        let math = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("abs", NativeFunction::MathAbs),
            ("ceil", NativeFunction::MathCeil),
            ("floor", NativeFunction::MathFloor),
            ("max", NativeFunction::MathMax),
            ("min", NativeFunction::MathMin),
            ("pow", NativeFunction::MathPow),
            ("random", NativeFunction::MathRandom),
            ("round", NativeFunction::MathRound),
            ("sqrt", NativeFunction::MathSqrt),
            ("sin", NativeFunction::MathOp(MathOp::Sin)),
            ("cos", NativeFunction::MathOp(MathOp::Cos)),
            ("tan", NativeFunction::MathOp(MathOp::Tan)),
            ("asin", NativeFunction::MathOp(MathOp::Asin)),
            ("acos", NativeFunction::MathOp(MathOp::Acos)),
            ("atan", NativeFunction::MathOp(MathOp::Atan)),
            ("sinh", NativeFunction::MathOp(MathOp::Sinh)),
            ("cosh", NativeFunction::MathOp(MathOp::Cosh)),
            ("tanh", NativeFunction::MathOp(MathOp::Tanh)),
            ("asinh", NativeFunction::MathOp(MathOp::Asinh)),
            ("acosh", NativeFunction::MathOp(MathOp::Acosh)),
            ("atanh", NativeFunction::MathOp(MathOp::Atanh)),
            ("log", NativeFunction::MathOp(MathOp::Log)),
            ("log2", NativeFunction::MathOp(MathOp::Log2)),
            ("log10", NativeFunction::MathOp(MathOp::Log10)),
            ("log1p", NativeFunction::MathOp(MathOp::Log1p)),
            ("exp", NativeFunction::MathOp(MathOp::Exp)),
            ("expm1", NativeFunction::MathOp(MathOp::Expm1)),
            ("sign", NativeFunction::MathOp(MathOp::Sign)),
            ("trunc", NativeFunction::MathOp(MathOp::Trunc)),
            ("cbrt", NativeFunction::MathOp(MathOp::Cbrt)),
            ("fround", NativeFunction::MathOp(MathOp::Fround)),
            ("clz32", NativeFunction::MathOp(MathOp::Clz32)),
            ("imul", NativeFunction::MathOp(MathOp::Imul)),
            ("atan2", NativeFunction::MathOp(MathOp::Atan2)),
            ("hypot", NativeFunction::MathOp(MathOp::Hypot)),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[math.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(method),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
        for (name, value) in [
            ("PI", std::f64::consts::PI),
            ("E", std::f64::consts::E),
            ("LN2", std::f64::consts::LN_2),
            ("LN10", std::f64::consts::LN_10),
            ("LOG2E", std::f64::consts::LOG2_E),
            ("LOG10E", std::f64::consts::LOG10_E),
            ("SQRT2", std::f64::consts::SQRT_2),
            ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
        ] {
            objects[math.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Number(value),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
        }
        objects[global.0].properties.insert(
            "Math".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(math),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    fn install_json(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        let json = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("parse", NativeFunction::JsonParse),
            ("stringify", NativeFunction::JsonStringify),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::BoundFunction {
                    function,
                    receiver: json,
                },
                ..JsObject::default()
            });
            objects[json.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[global.0].properties.insert(
            "JSON".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(json),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// Installs the network surface: `fetch()`, `Response`, and
    /// `XMLHttpRequest`.
    ///
    /// Transfers are only queued here; the embedding drains
    /// `take_pending_fetch_requests` and completes each id through
    /// `settle_fetch`.
    #[allow(
        clippy::too_many_lines,
        reason = "bootstrap tables read best as a single listing"
    )]
    fn install_fetch(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        Self::define_global_function(objects, global, "fetch", NativeFunction::GlobalFetch);
        Self::define_global_function(
            objects,
            global,
            "structuredClone",
            NativeFunction::GlobalStructuredClone,
        );

        Self::install_network_constructor(
            objects,
            global,
            object_prototype,
            function_prototype,
            "AbortController",
            ObjectHost::AbortControllerConstructor,
            &[("abort", NativeFunction::AbortControllerAbort)],
        );
        Self::install_network_constructor(
            objects,
            global,
            object_prototype,
            function_prototype,
            "FormData",
            ObjectHost::FormDataConstructor,
            &[
                ("append", NativeFunction::FormDataAppend),
                ("get", NativeFunction::FormDataGet),
                ("set", NativeFunction::FormDataSet),
                ("has", NativeFunction::FormDataHas),
                ("delete", NativeFunction::FormDataDelete),
                ("entries", NativeFunction::FormDataEntries),
            ],
        );

        // `Response` instances materialize when a transfer settles; the
        // constructor stays callable for feature detection and shims.
        let response_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("text", NativeFunction::ResponseText),
            ("json", NativeFunction::ResponseJson),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[response_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        let response_constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::ResponseConstructor,
            ..JsObject::default()
        });
        objects[response_constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(response_prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[response_prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(response_constructor)),
        );
        objects[global.0].properties.insert(
            "Response".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(response_constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );

        // Classic-subset `XMLHttpRequest`: open/setRequestHeader/send plus
        // the readyState constants real bundle code probes for.
        let xhr_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("open", NativeFunction::XhrOpen),
            ("setRequestHeader", NativeFunction::XhrSetRequestHeader),
            ("send", NativeFunction::XhrSend),
            ("getResponseHeader", NativeFunction::XhrGetResponseHeader),
            (
                "getAllResponseHeaders",
                NativeFunction::XhrGetAllResponseHeaders,
            ),
            ("addEventListener", NativeFunction::XhrAddEventListener),
            (
                "removeEventListener",
                NativeFunction::XhrRemoveEventListener,
            ),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[xhr_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        for (name, value) in [
            ("UNSENT", 0.0),
            ("OPENED", 1.0),
            ("HEADERS_RECEIVED", 2.0),
            ("LOADING", 3.0),
            ("DONE", 4.0),
        ] {
            objects[xhr_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Number(value)),
            );
        }
        let xhr_constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::XmlHttpRequestConstructor,
            ..JsObject::default()
        });
        objects[xhr_constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(xhr_prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        for (name, value) in [
            ("UNSENT", 0.0),
            ("OPENED", 1.0),
            ("HEADERS_RECEIVED", 2.0),
            ("LOADING", 3.0),
            ("DONE", 4.0),
        ] {
            objects[xhr_constructor.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Number(value)),
            );
        }
        objects[xhr_prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(xhr_constructor)),
        );
        objects[global.0].properties.insert(
            "XMLHttpRequest".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(xhr_constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );

        // File API Blob. It is deliberately a small, synchronous value
        // object: network ownership remains with the browser embedding.
        let blob_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("text", NativeFunction::BlobText),
            ("arrayBuffer", NativeFunction::BlobArrayBuffer),
            ("slice", NativeFunction::BlobSlice),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[blob_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        let blob_constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::BlobConstructor,
            ..JsObject::default()
        });
        objects[blob_constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(blob_prototype)),
        );
        objects[blob_prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(blob_constructor)),
        );
        objects[global.0].properties.insert(
            "Blob".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(blob_constructor)),
        );
    }

    fn install_network_constructor(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
        name: &str,
        host: ObjectHost,
        methods: &[(&str, NativeFunction)],
    ) {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (method_name, native) in methods {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(*native),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                (*method_name).to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
            if name == "FormData" && *method_name == "entries" {
                let symbol = JsSymbol::well_known("@@iterator");
                objects[prototype.0].symbols.insert(
                    symbol.id(),
                    (symbol, PropertyDescriptor::builtin(JsValue::Object(method))),
                );
            }
        }
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host,
            ..JsObject::default()
        });
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(prototype)),
        );
        objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(constructor)),
        );
        objects[global.0].properties.insert(
            name.to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// Installs the ES Proxy constructor and the Reflect operations used by
    /// proxy traps in application frameworks (Vue, React tooling, etc.).
    fn install_proxy_reflect(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        let proxy_constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::ProxyConstructor,
            ..JsObject::default()
        });
        objects[proxy_constructor.0].properties.insert(
            "length".to_owned(),
            PropertyDescriptor::builtin(JsValue::Number(2.0)),
        );
        objects[global.0].properties.insert(
            "Proxy".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(proxy_constructor)),
        );
        // `Proxy.revocable` (ECMA-262 28.2.2.1) is a method of the constructor.
        let revocable = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::ProxyRevocable),
            ..JsObject::default()
        });
        objects[revocable.0].properties.insert(
            "length".to_owned(),
            PropertyDescriptor {
                value: JsValue::Number(2.0),
                writable: false,
                getter: None,
                setter: None,
                enumerable: false,
                configurable: true,
            },
        );
        objects[proxy_constructor.0].properties.insert(
            "revocable".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(revocable)),
        );

        let reflect = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("get", NativeFunction::ReflectGet),
            ("set", NativeFunction::ReflectSet),
            ("has", NativeFunction::ReflectHas),
            ("deleteProperty", NativeFunction::ReflectDeleteProperty),
            ("ownKeys", NativeFunction::ReflectOwnKeys),
            (
                "getOwnPropertyDescriptor",
                NativeFunction::ReflectGetOwnPropertyDescriptor,
            ),
            ("defineProperty", NativeFunction::ReflectDefineProperty),
            ("construct", NativeFunction::ReflectConstruct),
            ("apply", NativeFunction::ReflectApply),
            ("getPrototypeOf", NativeFunction::ReflectGetPrototypeOf),
            ("setPrototypeOf", NativeFunction::ReflectSetPrototypeOf),
            ("isExtensible", NativeFunction::ReflectIsExtensible),
            (
                "preventExtensions",
                NativeFunction::ReflectPreventExtensions,
            ),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[reflect.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[global.0].properties.insert(
            "Reflect".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(reflect)),
        );
    }

    /// Installs the `Video` (`HTMLVideoElement`) surface: a global
    /// constructor in the `Image`/`XMLHttpRequest` style whose instances
    /// expose `play`/`pause`/`load`/`canPlayType` and plain playback
    /// properties.
    ///
    /// Media loads queue through the shared network pending queue; the
    /// embedding drains them with `take_pending_fetch_requests` and completes
    /// them through `settle_video_fetch`.
    fn install_video(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        let video_prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("play", NativeFunction::VideoPlay),
            ("pause", NativeFunction::VideoPause),
            ("load", NativeFunction::VideoLoad),
            ("canPlayType", NativeFunction::VideoCanPlayType),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[video_prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        let video_constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::VideoConstructor,
            ..JsObject::default()
        });
        objects[video_constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(video_prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[video_prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(video_constructor)),
        );
        objects[global.0].properties.insert(
            "Video".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(video_constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// The statics on `Promise` and the `get [Symbol.species]` accessor. Each
    /// static is a plain native function, so the receiver it is called on reaches
    /// it as `this`: the spec's generic `NewPromiseCapability(C)` depends on that.
    fn install_promise_statics(
        objects: &mut Vec<JsObject>,
        promise: ObjectId,
        function_prototype: ObjectId,
    ) {
        for (name, function, length) in [
            ("resolve", NativeFunction::PromiseResolve, 1.0),
            ("reject", NativeFunction::PromiseReject, 1.0),
            ("withResolvers", NativeFunction::PromiseWithResolvers, 0.0),
            ("try", NativeFunction::PromiseTry, 1.0),
            ("all", NativeFunction::PromiseAll, 1.0),
            ("allSettled", NativeFunction::PromiseAllSettled, 1.0),
            ("allKeyed", NativeFunction::PromiseAllKeyed, 1.0),
            (
                "allSettledKeyed",
                NativeFunction::PromiseAllSettledKeyed,
                1.0,
            ),
            ("any", NativeFunction::PromiseAny, 1.0),
            ("race", NativeFunction::PromiseRace, 1.0),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                properties: Self::function_metadata(name, length),
                ..JsObject::default()
            });
            objects[promise.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        // ECMA-262 27.2.5.4.9: `get Promise[@@species]` returns `this`.
        let species_getter = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::PromiseSpecies),
            properties: Self::function_metadata("get [Symbol.species]", 0.0),
            ..JsObject::default()
        });
        let species = JsSymbol::well_known("@@species");
        objects[promise.0].symbols.insert(
            species.id(),
            (
                species,
                PropertyDescriptor {
                    getter: Some(species_getter),
                    setter: None,
                    value: JsValue::Undefined,
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            ),
        );
    }

    /// Install `Promise`, returning the constructor and `Promise.prototype`.
    fn install_promise(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
        error_prototype: ObjectId,
    ) -> (ObjectId, ObjectId) {
        let promise = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::PromiseConstructor,
            ..JsObject::default()
        });
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function, length) in [
            ("then", NativeFunction::PromiseThen, 2.0),
            ("catch", NativeFunction::PromiseCatch, 1.0),
            ("finally", NativeFunction::PromiseFinally, 1.0),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                prototype: Some(function_prototype),
                host: ObjectHost::NativeFunction(function),
                properties: Self::function_metadata(name, length),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(promise)),
        );
        objects[promise.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        Self::install_promise_statics(objects, promise, function_prototype);
        Self::install_aggregate_error(objects, global, function_prototype, error_prototype);
        // `Promise.prototype[Symbol.toStringTag] === "Promise"`
        {
            let tag = JsSymbol::well_known("@@toStringTag");
            objects[prototype.0].symbols.insert(
                tag.id(),
                (
                    tag,
                    // ECMA-262 27.2.5.5: the tag is read-only, unlike the methods.
                    PropertyDescriptor {
                        writable: false,
                        ..PropertyDescriptor::builtin(JsValue::String("Promise".to_owned()))
                    },
                ),
            );
        }
        objects[global.0].properties.insert(
            "Promise".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(promise),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        (promise, prototype)
    }

    /// `AggregateError`, the rejection reason `Promise.any` produces.
    ///
    /// `errors` is an own property of each *instance*, not an accessor on the
    /// prototype: the specification defines it that way, and a prototype
    /// accessor would need a hidden slot that this engine's objects do not have.
    /// The prototype hangs off `%Error.prototype%`, so `instanceof Error` and
    /// `toString()` follow the ordinary error contract and `message` is the
    /// conventional empty string when none was given.
    fn install_aggregate_error(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        function_prototype: ObjectId,
        error_prototype: ObjectId,
    ) {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(error_prototype),
            ..JsObject::default()
        });
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::AggregateErrorConstructor,
            ..JsObject::default()
        });
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        objects[constructor.0].properties.insert(
            "name".to_owned(),
            PropertyDescriptor::builtin(JsValue::String("AggregateError".to_owned())),
        );
        objects[constructor.0].properties.insert(
            "length".to_owned(),
            PropertyDescriptor::builtin(JsValue::Number(2.0)),
        );
        for (name, value) in [("name", "AggregateError"), ("message", "")] {
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::String(value.to_owned()),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
        objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(constructor)),
        );
        objects[global.0].properties.insert(
            "AggregateError".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// `SuppressedError` (ECMA-262 explicit resource management), returning
    /// `%SuppressedError.prototype%`. The constructor's `[[Prototype]]` is
    /// `%Error%` and the prototype inherits `%Error.prototype%`. Instances get
    /// `error` and `suppressed` as own, non-enumerable properties, which the
    /// runtime defines itself (`runtime::builtins::explicit_resource`).
    fn install_suppressed_error(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        function_prototype: ObjectId,
        error_prototype: ObjectId,
    ) -> ObjectId {
        let error_constructor = match objects[error_prototype.0].properties.get("constructor") {
            Some(PropertyDescriptor {
                value: JsValue::Object(constructor),
                ..
            }) => *constructor,
            _ => function_prototype,
        };
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(error_prototype),
            ..JsObject::default()
        });
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(error_constructor),
            host: ObjectHost::SuppressedErrorConstructor,
            properties: Self::function_metadata("SuppressedError", 3.0),
            ..JsObject::default()
        });
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        for (name, value) in [("name", "SuppressedError"), ("message", "")] {
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::String(value.to_owned())),
            );
        }
        objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(constructor)),
        );
        objects[global.0].properties.insert(
            "SuppressedError".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        prototype
    }

    /// `DisposableStack` or `AsyncDisposableStack` (ECMA-262 explicit resource
    /// management), returning the prototype. It holds the constructor's
    /// `prototype`, the five methods, the `disposed` getter, `constructor`, and
    /// `@@toStringTag`. The disposal method is one function object under its
    /// name and under its symbol (`Symbol.dispose` or `Symbol.asyncDispose`).
    fn install_disposable_stack(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
        kind: DisposeStackKind,
    ) -> ObjectId {
        let name = if kind.is_async() {
            "AsyncDisposableStack"
        } else {
            "DisposableStack"
        };
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        let constructor = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::DisposableStackConstructor(kind),
            properties: Self::function_metadata(name, 0.0),
            ..JsObject::default()
        });
        objects[constructor.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
        Self::install_disposable_stack_methods(objects, prototype, function_prototype, kind);
        objects[prototype.0].properties.insert(
            "constructor".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(constructor)),
        );
        // `@@toStringTag` is read-only, like `Promise.prototype`'s.
        let tag = JsSymbol::well_known("@@toStringTag");
        objects[prototype.0].symbols.insert(
            tag.id(),
            (
                tag,
                PropertyDescriptor {
                    writable: false,
                    ..PropertyDescriptor::builtin(JsValue::String(name.to_owned()))
                },
            ),
        );
        objects[global.0].properties.insert(
            name.to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(constructor),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        prototype
    }

    /// `Symbol.dispose` and `Symbol.asyncDispose` on the `Symbol` constructor
    /// (ECMA-262 explicit resource management): non-writable, non-configurable
    /// data properties, unlike the well-known symbols beside them.
    fn install_dispose_symbols(objects: &mut [JsObject], symbol_constructor: ObjectId) {
        for (name, key) in [("dispose", "@@dispose"), ("asyncDispose", "@@asyncDispose")] {
            objects[symbol_constructor.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Symbol(JsSymbol::well_known(key)),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
        }
    }

    /// The five methods and the `disposed` getter of a stack prototype. The
    /// disposal method is the same object under `dispose` (or `disposeAsync`)
    /// and under `@@dispose` (or `@@asyncDispose`).
    fn install_disposable_stack_methods(
        objects: &mut Vec<JsObject>,
        prototype: ObjectId,
        function_prototype: ObjectId,
        kind: DisposeStackKind,
    ) {
        let (dispose_name, dispose_symbol) = if kind.is_async() {
            ("disposeAsync", "@@asyncDispose")
        } else {
            ("dispose", "@@dispose")
        };
        for (method, operation, length) in [
            ("adopt", DisposeStackOp::Adopt, 2.0),
            ("defer", DisposeStackOp::Defer, 1.0),
            (dispose_name, DisposeStackOp::Dispose, 0.0),
            ("move", DisposeStackOp::Move, 0.0),
            ("use", DisposeStackOp::Use, 1.0),
        ] {
            let function = Self::push_native_method(
                objects,
                function_prototype,
                NativeFunction::DisposeStack(operation, kind),
                method,
                length,
            );
            objects[prototype.0].properties.insert(
                method.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(function)),
            );
            if operation == DisposeStackOp::Dispose {
                let symbol = JsSymbol::well_known(dispose_symbol);
                objects[prototype.0].symbols.insert(
                    symbol.id(),
                    (
                        symbol,
                        PropertyDescriptor::builtin(JsValue::Object(function)),
                    ),
                );
            }
        }
        let getter = Self::push_native_method(
            objects,
            function_prototype,
            NativeFunction::DisposeStack(DisposeStackOp::Disposed, kind),
            "get disposed",
            0.0,
        );
        objects[prototype.0].properties.insert(
            "disposed".to_owned(),
            PropertyDescriptor {
                getter: Some(getter),
                setter: None,
                value: JsValue::Undefined,
                writable: false,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// A built-in function object running `function`, with its `name` and
    /// `length` set explicitly, because the arity table does not know it.
    fn push_native_method(
        objects: &mut Vec<JsObject>,
        function_prototype: ObjectId,
        function: NativeFunction,
        name: &str,
        length: f64,
    ) -> ObjectId {
        let id = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(function),
            properties: Self::function_metadata(name, length),
            ..JsObject::default()
        });
        id
    }

    /// `Array.of`, the `Array[Symbol.species]` getter (ECMA-262 23.1.2.5) and
    /// `Array.prototype[Symbol.unscopables]` (ECMA-262 23.1.3.41), which is a
    /// null-prototype object naming the methods `with` must not shadow.
    fn install_array_statics(
        objects: &mut Vec<JsObject>,
        array: ObjectId,
        prototype: ObjectId,
        function_prototype: ObjectId,
    ) {
        let from = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::ArrayFrom),
            ..JsObject::default()
        });
        objects[array.0].properties.insert(
            "from".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(from)),
        );
        let of = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::ArrayOf),
            ..JsObject::default()
        });
        objects[array.0].properties.insert(
            "of".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(of)),
        );
        let from_async = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::ArrayFromAsync),
            ..JsObject::default()
        });
        objects[array.0].properties.insert(
            "fromAsync".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(from_async)),
        );
        let species_getter = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::NativeFunction(NativeFunction::ArraySpecies),
            ..JsObject::default()
        });
        let species = JsSymbol::well_known("@@species");
        objects[array.0].symbols.insert(
            species.id(),
            (
                species,
                PropertyDescriptor {
                    getter: Some(species_getter),
                    setter: None,
                    value: JsValue::Undefined,
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            ),
        );
        let unscopables = ObjectId(objects.len());
        objects.push(JsObject::default());
        for name in [
            "at",
            "copyWithin",
            "entries",
            "fill",
            "find",
            "findIndex",
            "findLast",
            "findLastIndex",
            "flat",
            "flatMap",
            "includes",
            "keys",
            "toReversed",
            "toSorted",
            "toSpliced",
            "values",
        ] {
            objects[unscopables.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::data(JsValue::Boolean(true)),
            );
        }
        let unscopables_symbol = JsSymbol::well_known("@@unscopables");
        objects[prototype.0].symbols.insert(
            unscopables_symbol.id(),
            (
                unscopables_symbol,
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(unscopables),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            ),
        );
    }

    fn install_array(
        objects: &mut Vec<JsObject>,
        global: ObjectId,
        object_prototype: ObjectId,
        function_prototype: ObjectId,
    ) -> ObjectId {
        let prototype = ObjectId(objects.len());
        objects.push(JsObject {
            prototype: Some(object_prototype),
            ..JsObject::default()
        });
        for (name, function) in [
            ("push", NativeFunction::ArrayPush),
            ("pop", NativeFunction::ArrayPop),
            ("join", NativeFunction::ArrayJoin),
            ("indexOf", NativeFunction::ArrayIndexOf),
            ("slice", NativeFunction::ArraySlice),
            ("splice", NativeFunction::ArraySplice),
            ("reverse", NativeFunction::ArrayReverse),
            ("sort", NativeFunction::ArraySort),
            ("concat", NativeFunction::ArrayConcat),
            ("shift", NativeFunction::ArrayShift),
            ("unshift", NativeFunction::ArrayUnshift),
            ("forEach", NativeFunction::ArrayForEach),
            ("map", NativeFunction::ArrayMap),
            ("filter", NativeFunction::ArrayFilter),
            ("some", NativeFunction::ArraySome),
            ("find", NativeFunction::ArrayFind),
            ("findIndex", NativeFunction::ArrayFindIndex),
            ("findLast", NativeFunction::ArrayFindLast),
            ("findLastIndex", NativeFunction::ArrayFindLastIndex),
            ("every", NativeFunction::ArrayEvery),
            ("includes", NativeFunction::ArrayIncludes),
            ("at", NativeFunction::ArrayAt),
            ("flat", NativeFunction::ArrayFlat),
            ("reduce", NativeFunction::ArrayReduce),
            ("reduceRight", NativeFunction::ArrayReduceRight),
            ("toString", NativeFunction::ArrayPrototypeToString),
            ("toLocaleString", NativeFunction::ArrayToLocaleString),
            ("lastIndexOf", NativeFunction::ArrayLastIndexOf),
            ("flatMap", NativeFunction::ArrayFlatMap),
            ("fill", NativeFunction::ArrayFill),
            ("copyWithin", NativeFunction::ArrayCopyWithin),
            ("toSorted", NativeFunction::ArrayToSorted),
            ("toReversed", NativeFunction::ArrayToReversed),
            ("toSpliced", NativeFunction::ArrayToSpliced),
            ("with", NativeFunction::ArrayWith),
            ("values", NativeFunction::ArrayValues),
            ("keys", NativeFunction::ArrayKeys),
            ("entries", NativeFunction::ArrayEntries),
        ] {
            let method = ObjectId(objects.len());
            objects.push(JsObject {
                host: ObjectHost::NativeFunction(function),
                ..JsObject::default()
            });
            objects[prototype.0].properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::Object(method)),
            );
        }
        // `Array.prototype[Symbol.iterator]` is the same function object as
        // `values`, so iterator helpers work on arrays directly.
        if let Some(values) = objects[prototype.0].properties.get("values")
            && let JsValue::Object(values) = values.value
        {
            let symbol = JsSymbol::well_known("@@iterator");
            objects[prototype.0].symbols.insert(
                symbol.id(),
                (symbol, PropertyDescriptor::builtin(JsValue::Object(values))),
            );
        }
        let array = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::ArrayConstructor,
            ..JsObject::default()
        });
        let is_array = ObjectId(objects.len());
        objects.push(JsObject {
            host: ObjectHost::BoundFunction {
                function: NativeFunction::ArrayIsArray,
                receiver: array,
            },
            ..JsObject::default()
        });
        objects[array.0].properties.insert(
            "isArray".to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(is_array)),
        );
        Self::install_array_statics(objects, array, prototype, function_prototype);
        objects[array.0].properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor::data(JsValue::Object(prototype)),
        );
        objects[global.0].properties.insert(
            "Array".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(array),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        prototype
    }

    #[must_use]
    pub const fn global_object(&self) -> ObjectId {
        self.global
    }

    #[must_use]
    pub const fn document_object(&self) -> ObjectId {
        self.document
    }

    #[must_use]
    pub fn object(&self, object: ObjectId) -> Option<&JsObject> {
        self.objects.get(object.0)
    }

    pub(crate) fn object_mut(&mut self, object: ObjectId) -> Option<&mut JsObject> {
        self.objects.get_mut(object.0)
    }

    /// Allocate an ordinary object with an optional prototype.
    pub fn create_object(&mut self, prototype: Option<ObjectId>) -> ObjectId {
        self.allocate(JsObject {
            prototype,
            ..JsObject::default()
        })
    }

    pub(crate) fn create_ordinary_object(&mut self) -> ObjectId {
        self.create_object(Some(self.object_prototype))
    }

    pub(crate) fn create_error(
        &mut self,
        prototype: ObjectId,
        message: Option<String>,
    ) -> ObjectId {
        // The `Error` host is the engine's stand-in for the spec's `[[ErrorData]]`
        // slot, which is what `Object.prototype.toString` reads to answer
        // `[object Error]` for an error instance rather than `[object Object]`.
        let error = self.allocate(JsObject {
            prototype: Some(prototype),
            host: ObjectHost::ErrorInstance,
            ..JsObject::default()
        });
        if let Some(message) = message {
            self.objects[error.0].properties.insert(
                "message".to_owned(),
                PropertyDescriptor::builtin(JsValue::String(message)),
            );
        }
        error
    }

    pub(crate) fn create_array(&mut self) -> ObjectId {
        let array = self.allocate(JsObject {
            prototype: Some(self.array_prototype),
            host: ObjectHost::Array,
            ..JsObject::default()
        });
        self.objects[array.0].properties.insert(
            "length".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Number(0.0),
                writable: true,
                enumerable: false,
                configurable: false,
            },
        );
        array
    }

    /// Define or replace an own data property.
    ///
    /// Returns `false` if a non-configurable property prevents replacement or
    /// the object does not exist.
    pub fn define_property(
        &mut self,
        object: ObjectId,
        key: impl Into<String>,
        descriptor: PropertyDescriptor,
    ) -> bool {
        let Some(target) = self.objects.get_mut(object.0) else {
            return false;
        };
        let key = key.into();
        // ECMA-262 §10.4.3.2: a String exotic object rejects every
        // `[[DefineOwnProperty]]` whose key is a canonical numeric index string
        // or `"length"`; those slots are non-configurable and non-writable, so
        // the caller's ordinary property machinery reports the failure.
        if matches!(target.host, ObjectHost::StringPrimitive(_))
            && (key == "length" || is_canonical_index(&key))
        {
            return false;
        }
        if let Some(current) = target.properties.get(&key) {
            if !current.configurable && !non_configurable_redefinition_allowed(current, &descriptor)
            {
                return false;
            }
        } else if !target.extensible {
            return false;
        }
        // ECMA-262 10.4.2.1: an array index at or past an Array's length grows
        // the length to cover it, and a read-only length refuses it.
        let mut grown_length = None;
        if matches!(target.host, ObjectHost::Array)
            && let Ok(index) = key.parse::<u32>()
            && index.to_string() == key
            && index < u32::MAX
            && let Some(length) = target.properties.get("length")
            && let JsValue::Number(current) = length.value
            && f64::from(index) >= current
        {
            if !length.writable {
                return false;
            }
            grown_length = Some(f64::from(index) + 1.0);
        }
        if let Some(new_length) = grown_length
            && let Some(length) = target.properties.get_mut("length")
        {
            length.value = JsValue::Number(new_length);
        }
        // Integer indices enumerate in ascending order, so only named keys
        // take a place in the insertion order.
        if !target.properties.contains_key(&key) && !is_canonical_index(&key) {
            target.key_order.push(key.clone());
        }
        target.properties.insert(key, descriptor);
        true
    }

    /// [[`GetPrototypeOf`]].
    #[must_use]
    pub(crate) fn get_prototype(&self, object: ObjectId) -> Option<ObjectId> {
        self.objects.get(object.0).and_then(JsObject::prototype)
    }

    /// The realm's `%Object.prototype%`.
    #[must_use]
    pub(crate) const fn object_prototype(&self) -> ObjectId {
        self.object_prototype
    }

    /// The realm's `%Function.prototype%`.
    pub(crate) const fn function_prototype(&self) -> ObjectId {
        self.function_prototype
    }

    /// The realm's `%MediaQueryList.prototype%`, which `matchMedia` stamps onto
    /// each list it creates.
    #[must_use]
    pub(crate) const fn media_query_list_prototype(&self) -> ObjectId {
        self.media_query_list_prototype
    }

    /// Make a class constructor's `prototype` property non-writable, as
    /// `ClassDefinitionEvaluation` requires (ordinary functions keep a
    /// writable prototype).
    pub(crate) fn configure_class_prototype(&mut self, function: ObjectId, prototype: ObjectId) {
        let Some(target) = self.objects.get_mut(function.0) else {
            return;
        };
        target.properties.insert(
            "prototype".to_owned(),
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(prototype),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        );
    }

    /// Own private method/accessor descriptor, if present on `object` itself.
    #[must_use]
    pub(crate) fn own_private_method(
        &self,
        object: ObjectId,
        id: u64,
    ) -> Option<PropertyDescriptor> {
        self.objects
            .get(object.0)
            .and_then(|target| target.private_method(id).cloned())
    }

    pub(crate) fn set_private_field(&mut self, object: ObjectId, id: u64, value: JsValue) {
        if let Some(target) = self.objects.get_mut(object.0) {
            target.set_private_field(id, value);
        }
    }

    #[must_use]
    pub(crate) fn private_field(&self, object: ObjectId, id: u64) -> Option<&JsValue> {
        self.objects.get(object.0)?.private_field(id)
    }

    #[must_use]
    pub(crate) fn has_private_field(&self, object: ObjectId, id: u64) -> bool {
        self.objects
            .get(object.0)
            .is_some_and(|target| target.has_private_field(id))
    }

    pub(crate) fn define_private_method(
        &mut self,
        object: ObjectId,
        id: u64,
        descriptor: PropertyDescriptor,
    ) {
        if let Some(target) = self.objects.get_mut(object.0) {
            target.define_private_method(id, descriptor);
        }
    }

    /// [[`SetPrototypeOf`]] for ordinary objects (class inheritance wiring).
    pub(crate) fn set_prototype(&mut self, object: ObjectId, prototype: Option<ObjectId>) -> bool {
        let Some(target) = self.objects.get(object.0) else {
            return false;
        };
        if target.prototype == prototype {
            return true;
        }
        if !target.extensible {
            return false;
        }
        let mut candidate = prototype;
        for _ in 0..self.objects.len() {
            let Some(current) = candidate else {
                self.objects[object.0].prototype = prototype;
                return true;
            };
            if current == object {
                return false;
            }
            candidate = match self.objects.get(current.0) {
                Some(parent) => parent.prototype,
                None => return false,
            };
        }
        // Existing corrupt cycles should not be extended by another write.
        false
    }

    /// A private method or accessor of `object` for `id`. It is an own element
    /// that the class installed on the instance (ECMA-262 7.3.32
    /// `PrivateElementFind`), so a prototype never supplies it.
    #[must_use]
    pub(crate) fn find_private_method(
        &self,
        object: ObjectId,
        id: u64,
    ) -> Option<PropertyDescriptor> {
        self.own_private_method(object, id)
    }

    /// Every private method and accessor installed as an own element of `object`.
    #[must_use]
    pub(crate) fn private_method_entries(
        &self,
        object: ObjectId,
    ) -> Vec<(u64, PropertyDescriptor)> {
        self.objects.get(object.0).map_or_else(Vec::new, |target| {
            target
                .private_methods
                .iter()
                .map(|(id, descriptor)| (*id, descriptor.clone()))
                .collect()
        })
    }

    /// Whether `object` already holds a private field or method for `id`, which
    /// a second initialization of the same element must reject (ECMA-262
    /// 7.3.29 `PrivateFieldAdd`, 7.3.31 `PrivateMethodOrAccessorAdd`).
    #[must_use]
    pub(crate) fn has_own_private_element(&self, object: ObjectId, id: u64) -> bool {
        self.objects.get(object.0).is_some_and(|target| {
            target.has_private_field(id) || target.private_method(id).is_some()
        })
    }

    /// `Object.preventExtensions`: new own properties are rejected.
    pub(crate) fn prevent_extensions(&mut self, object: ObjectId) -> bool {
        let Some(target) = self.objects.get_mut(object.0) else {
            return false;
        };
        target.extensible = false;
        true
    }

    #[must_use]
    pub(crate) fn is_extensible(&self, object: ObjectId) -> bool {
        self.objects
            .get(object.0)
            .is_some_and(|target| target.extensible)
    }

    #[must_use]
    pub fn global(&self, key: &str) -> Option<JsValue> {
        self.get_property(self.global, key)
    }

    pub(crate) fn set_global(&mut self, key: String, value: JsValue) -> bool {
        self.set_property(self.global, key, value)
    }

    /// Number of live (non-swept) object slots. Swept slots are rewritten to
    /// empty ordinary objects whose identities are never reused.
    pub(crate) fn object_count(&self) -> usize {
        self.objects.len().saturating_sub(self.swept_objects)
    }

    /// Fixed identity roots that must survive every collection: the global
    /// realm wrappers and every existing DOM/platform wrapper identity.
    pub(crate) fn gc_identity_roots(&self) -> Vec<ObjectId> {
        let mut roots = Vec::with_capacity(
            4 + self.node_wrappers.len()
                + self.class_list_wrappers.len()
                + self.style_declaration_wrappers.len()
                + self.dataset_wrappers.len(),
        );
        roots.push(self.global);
        roots.push(self.document);
        // The iterator prototypes are only reachable through helper objects,
        // which may all be garbage at collection time; keep them rooted.
        roots.push(self.iterator_prototype);
        roots.push(self.generator_prototype);
        roots.push(self.async_generator_prototype);
        roots.push(self.async_generator_function_prototype);
        roots.push(self.async_from_sync_iterator_prototype);
        roots.push(self.iterator_helper_prototype);
        roots.push(self.iterator_wrap_prototype);
        roots.push(self.regexp_string_iterator_prototype);
        // The Storage prototype is only reachable through the two area
        // objects, whose own keys are the caller's data.
        roots.push(self.storage_prototype);
        roots.push(self.disposable_stack_prototype);
        roots.push(self.async_disposable_stack_prototype);
        roots.push(self.suppressed_error_prototype);
        roots.push(self.media_query_list_prototype);
        roots.extend(self.node_wrappers.values().copied());
        roots.extend(self.class_list_wrappers.values().copied());
        roots.extend(self.style_declaration_wrappers.values().copied());
        roots.extend(self.dataset_wrappers.values().copied());
        roots
    }

    pub(crate) fn objects(&self) -> &[JsObject] {
        &self.objects
    }

    /// Replace every unmarked object slot with an empty tombstone so its
    /// property storage is released. Live identities never move or reuse, so
    /// a lingering reference to a swept slot observes an inert object rather
    /// than corrupted state. Returns the number of reclaimed slots.
    pub(crate) fn sweep_unmarked(&mut self, marked: &[bool]) -> usize {
        debug_assert_eq!(marked.len(), self.objects.len());
        let mut swept = 0usize;
        for (index, alive) in marked.iter().enumerate() {
            if !alive {
                self.objects[index] = JsObject::default();
                swept = swept.saturating_add(1);
            }
        }
        let reclaimed = swept.saturating_sub(self.swept_objects);
        self.swept_objects = swept;
        reclaimed
    }

    /// Replace the URL the global `location` object reports. `pushState` and
    /// `replaceState` use it, because the document URL changes without a load.
    pub(crate) fn set_location_url(&mut self, url: Url) {
        let Some(JsValue::Object(location)) = self.global("location") else {
            return;
        };
        // The components (`href`, `pathname`, ...) are data properties filled
        // in when the object is made, so they are rewritten along with the host.
        let Some(object) = self.object_mut(location) else {
            return;
        };
        for (name, value) in location_components(&url) {
            object.properties.insert(
                name.to_owned(),
                PropertyDescriptor::builtin(JsValue::String(value)),
            );
        }
        if let ObjectHost::Location(current) = &mut object.host {
            *current = url;
        }
    }

    pub(crate) fn host(&self, object: ObjectId) -> Option<ObjectHost> {
        self.objects.get(object.0).map(|object| object.host.clone())
    }

    pub(crate) fn host_mut(&mut self, object: ObjectId) -> Option<&mut ObjectHost> {
        self.objects
            .get_mut(object.0)
            .map(|object| &mut object.host)
    }

    /// Prototype-chain read that also reports the object the property was
    /// found on, so member resolution can distinguish a genuine override on
    /// an interface prototype from `Object.prototype`'s generic members.
    pub(crate) fn get_property_with_origin(
        &self,
        object: ObjectId,
        key: &str,
    ) -> Option<(JsValue, ObjectId)> {
        if let Some(descriptor) = self.string_exotic_descriptor(object, key) {
            return Some((descriptor.value, object));
        }
        let mut candidate = Some(object);
        let mut visited = 0usize;
        while let Some(id) = candidate {
            let current = self.objects.get(id.0)?;
            if let Some(property) = current.properties.get(key) {
                return Some((property.value.clone(), id));
            }
            candidate = current.prototype;
            visited = visited.saturating_add(1);
            if visited > self.objects.len() {
                return None;
            }
        }
        None
    }

    /// `[[GetOwnProperty]]` for a String exotic object, consulted by the
    /// prototype-chain readers so a wrapper's characters and `length` are
    /// visible without materialising them as real own properties. Returns
    /// `None` when the object carries an ordinary property of that name, which
    /// then shadows the exotic slot.
    fn string_exotic_descriptor(&self, object: ObjectId, key: &str) -> Option<PropertyDescriptor> {
        let target = self.objects.get(object.0)?;
        // The host discriminant is checked first: these readers sit on the
        // member-access hot path and almost no object is a String wrapper.
        let ObjectHost::StringPrimitive(text) = &target.host else {
            return None;
        };
        if target.properties.contains_key(key) {
            return None;
        }
        string_exotic_property(text, key)
    }

    pub(crate) fn get_property(&self, object: ObjectId, key: &str) -> Option<JsValue> {
        if let Some(descriptor) = self.string_exotic_descriptor(object, key) {
            return Some(descriptor.value);
        }
        let mut candidate = Some(object);
        let mut visited = 0usize;
        while let Some(id) = candidate {
            let current = self.objects.get(id.0)?;
            if let Some(property) = current.properties.get(key) {
                return Some(property.value.clone());
            }
            candidate = current.prototype;
            visited = visited.saturating_add(1);
            if visited > self.objects.len() {
                return None;
            }
        }
        None
    }

    /// Whether `object` is a `RegExp`, and if so its record index. Reads the host
    /// in place, without cloning it.
    pub(crate) fn regexp_index_of(&self, object: ObjectId) -> Option<usize> {
        match self.objects.get(object.0)?.host {
            ObjectHost::RegExp(index) => Some(index),
            _ => None,
        }
    }

    /// Whether `object` has an own accessor `key` with no setter, so that a
    /// [[Set]] of `key` fails. No descriptor is cloned.
    pub(crate) fn own_accessor_lacks_setter(&self, object: ObjectId, key: &str) -> bool {
        self.objects
            .get(object.0)
            .and_then(|record| record.properties.get(key))
            .is_some_and(|property| property.is_accessor() && property.setter.is_none())
    }

    /// The own property `key` of `object` when it is a data property: whether it
    /// is writable. `None` for an accessor or an absent property. No descriptor
    /// is cloned.
    pub(crate) fn own_writable_data(&self, object: ObjectId, key: &str) -> Option<bool> {
        let property = self.objects.get(object.0)?.properties.get(key)?;
        (!property.is_accessor()).then_some(property.writable)
    }

    /// The getter of the own accessor `key` of `object`, or `None` when the
    /// property is a data property, has no getter, or is absent. No descriptor
    /// is cloned.
    pub(crate) fn own_getter(&self, object: ObjectId, key: &str) -> Option<ObjectId> {
        let property = self.objects.get(object.0)?.properties.get(key)?;
        if property.is_accessor() {
            property.getter
        } else {
            None
        }
    }

    /// Whether `object` has an own property named `key`, without cloning it.
    pub(crate) fn has_own_string_key(&self, object: ObjectId, key: &str) -> bool {
        self.objects
            .get(object.0)
            .is_some_and(|target| target.properties.contains_key(key))
    }

    pub(crate) fn own_property(&self, object: ObjectId, key: &str) -> Option<PropertyDescriptor> {
        let target = self.objects.get(object.0)?;
        if let Some(descriptor) = target.properties.get(key) {
            return Some(descriptor.clone());
        }
        match &target.host {
            ObjectHost::StringPrimitive(text) => string_exotic_property(text, key),
            _ => None,
        }
    }

    pub(crate) fn own_symbol_property(
        &self,
        object: ObjectId,
        symbol: &JsSymbol,
    ) -> Option<PropertyDescriptor> {
        self.objects
            .get(object.0)?
            .symbols
            .get(&symbol.id())
            .map(|(_, descriptor)| descriptor.clone())
    }

    /// Prototype-chain descriptor lookup for a symbol-keyed property.
    pub(crate) fn get_symbol_descriptor(
        &self,
        object: ObjectId,
        symbol: &JsSymbol,
    ) -> Option<PropertyDescriptor> {
        let mut candidate = Some(object);
        let mut visited = 0usize;
        while let Some(id) = candidate {
            let current = self.objects.get(id.0)?;
            if let Some((_, descriptor)) = current.symbols.get(&symbol.id()) {
                return Some(descriptor.clone());
            }
            candidate = current.prototype;
            visited = visited.saturating_add(1);
            if visited > self.objects.len() {
                return None;
            }
        }
        None
    }

    /// Create or overwrite an own symbol-keyed property, honouring
    /// non-configurable descriptors and object extensibility. A
    /// non-configurable property accepts only the redefinitions that
    /// `ValidateAndApplyPropertyDescriptor` allows, exactly like the
    /// string-keyed `define_property`.
    pub(crate) fn define_symbol_property(
        &mut self,
        object: ObjectId,
        symbol: &JsSymbol,
        descriptor: PropertyDescriptor,
    ) -> bool {
        let Some(target) = self.objects.get_mut(object.0) else {
            return false;
        };
        if let Some((_, current)) = target.symbols.get(&symbol.id()) {
            if !current.configurable && !non_configurable_redefinition_allowed(current, &descriptor)
            {
                return false;
            }
        } else if !target.extensible {
            return false;
        }
        target
            .symbols
            .insert(symbol.id(), (symbol.clone(), descriptor));
        true
    }

    pub(crate) fn delete_symbol_property(&mut self, object: ObjectId, symbol: &JsSymbol) -> bool {
        let Some(target) = self.objects.get_mut(object.0) else {
            return false;
        };
        if target
            .symbols
            .get(&symbol.id())
            .is_some_and(|(_, descriptor)| !descriptor.configurable)
        {
            return false;
        }
        target.symbols.remove(&symbol.id()).is_some()
    }

    /// Own symbol-keyed property symbols, id order, for
    /// `Object.getOwnPropertySymbols`.
    pub(crate) fn own_symbols(&self, object: ObjectId) -> Option<Vec<JsSymbol>> {
        Some(
            self.objects
                .get(object.0)?
                .symbols
                .values()
                .map(|(symbol, _)| symbol.clone())
                .collect(),
        )
    }

    /// Prototype-chain descriptor lookup (`[[GetOwnProperty]]` along the
    /// chain): the raw descriptor, accessor slots included. Pure reads that
    /// must not run user code stay on `get_property`; accessor invocation
    /// belongs to the runtime's [[Get]]/[[Set]] layer.
    pub(crate) fn get_descriptor(&self, object: ObjectId, key: &str) -> Option<PropertyDescriptor> {
        if let Some(descriptor) = self.string_exotic_descriptor(object, key) {
            return Some(descriptor);
        }
        let mut candidate = Some(object);
        let mut visited = 0usize;
        while let Some(id) = candidate {
            let current = self.objects.get(id.0)?;
            if let Some(descriptor) = current.properties.get(key) {
                return Some(descriptor.clone());
            }
            candidate = current.prototype;
            visited = visited.saturating_add(1);
            if visited > self.objects.len() {
                return None;
            }
        }
        None
    }

    pub(crate) fn enumerable_own_properties(
        &self,
        object: ObjectId,
    ) -> Option<Vec<(String, JsValue)>> {
        // Reading through `own_property` keeps the String exotic object's
        // virtual indexed characters enumerable alongside real own properties.
        self.objects.get(object.0)?;
        let keys = self.own_property_names(object)?;
        let mut properties = Vec::new();
        for key in keys {
            let Some(descriptor) = self.own_property(object, &key) else {
                continue;
            };
            if descriptor.enumerable {
                properties.push((key, descriptor.value.clone()));
            }
        }
        Some(properties)
    }

    /// The enumerable own string keys of an object, in property order.
    ///
    /// `CopyDataProperties` reads each of these with `Get`, which can run an
    /// accessor, so the key list is handed back separately rather than paired
    /// with the descriptor's `value` slot — that slot is `undefined` for an
    /// accessor, which is how `{ ...source }` used to lose every getter.
    pub(crate) fn enumerable_own_keys(&self, object: ObjectId) -> Option<Vec<String>> {
        self.objects.get(object.0)?;
        let keys = self.own_property_names(object)?;
        Some(
            keys.into_iter()
                .filter(|key| {
                    self.own_property(object, key)
                        .is_some_and(|descriptor| descriptor.enumerable)
                })
                .collect(),
        )
    }

    pub(crate) fn own_property_names(&self, object: ObjectId) -> Option<Vec<String>> {
        let target = self.objects.get(object.0)?;
        // Integer indices ascend first; the remaining string keys follow
        // first-insertion order, then any bootstrap keys the order list
        // does not track.
        let mut indices = target
            .properties
            .keys()
            .filter(|key| is_canonical_index(key))
            .cloned()
            .collect::<Vec<_>>();
        if let ObjectHost::StringPrimitive(text) = &target.host {
            // ECMA-262 §10.4.3: a String exotic object lists its characters as
            // ascending integer indices ahead of the ordinary string keys.
            indices.extend(
                (0..utf16::utf16_length(text))
                    .map(|index| index.to_string())
                    .filter(|key| !target.properties.contains_key(key)),
            );
        }
        indices.sort_by_cached_key(|key| key.parse::<u32>().unwrap_or(0));
        indices.dedup();
        let ordered = target
            .key_order
            .iter()
            .filter(|key| target.properties.contains_key(*key) && !is_canonical_index(key))
            .cloned()
            .collect::<Vec<_>>();
        let untracked = target
            .properties
            .keys()
            .filter(|key| !is_canonical_index(key) && !target.key_order.contains(key))
            .cloned()
            .collect::<Vec<_>>();
        let mut names = indices;
        names.extend(ordered);
        names.extend(untracked);
        if matches!(target.host, ObjectHost::StringPrimitive(_)) {
            // `length` is the only non-index own key of a String exotic
            // object, and it sorts last.
            names.push("length".to_owned());
        }
        Some(names)
    }

    pub(crate) fn enumerable_property_names(&self, object: ObjectId) -> Option<Vec<String>> {
        let mut names = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut candidate = Some(object);
        let mut visited = 0usize;
        let mut first = true;
        while let Some(id) = candidate {
            let current = self.objects.get(id.0)?;
            for (key, descriptor) in &current.properties {
                if seen.insert(key.clone()) && descriptor.enumerable {
                    names.push(key.clone());
                }
            }
            // A String exotic object only contributes its enumerable indexed
            // characters here; `length` is not enumerable.
            if first && let ObjectHost::StringPrimitive(text) = &current.host {
                for index in 0..utf16::utf16_length(text) {
                    let key = index.to_string();
                    if !current.properties.contains_key(&key) && seen.insert(key.clone()) {
                        names.push(key);
                    }
                }
            }
            first = false;
            candidate = current.prototype;
            visited = visited.saturating_add(1);
            if visited > self.objects.len() {
                return None;
            }
        }
        Some(names)
    }

    /// Drop the `prototype` a user function is created with when it is a method
    /// rather than a constructor (ECMA-262 15.4.4): methods, getters and setters
    /// have none, and the slot is non-configurable, so it is removed directly.
    pub(crate) fn remove_method_prototype(&mut self, object: ObjectId) {
        if let Some(target) = self.objects.get_mut(object.0) {
            target.properties.remove("prototype");
            target.key_order.retain(|key| key != "prototype");
        }
    }

    pub(crate) fn delete_property(&mut self, object: ObjectId, key: &str) -> bool {
        let Some(target) = self.objects.get_mut(object.0) else {
            return false;
        };
        if target
            .properties
            .get(key)
            .is_some_and(|descriptor| !descriptor.configurable)
        {
            return false;
        }
        target.properties.remove(key);
        if !is_canonical_index(key) {
            // A truncating `length` deletes many index keys in a row, so only
            // named keys pay for a scan of the insertion order.
            target.key_order.retain(|ordered| ordered != key);
        }
        true
    }

    /// Define an own writable data property that is neither enumerable nor
    /// configurable, as a `RegExp` instance's `lastIndex` is (ECMA-262 22.2.7.1).
    pub(crate) fn define_hidden_data(&mut self, object: ObjectId, key: &str, value: JsValue) {
        if let Some(target) = self.objects.get_mut(object.0) {
            target.properties.insert(
                key.to_owned(),
                PropertyDescriptor {
                    value,
                    writable: true,
                    getter: None,
                    setter: None,
                    enumerable: false,
                    configurable: false,
                },
            );
        }
    }

    /// The intrinsic %RegExp.prototype%.
    pub(crate) fn regexp_prototype(&self) -> ObjectId {
        self.regexp_prototype
    }

    pub(crate) fn set_property(&mut self, object: ObjectId, key: String, value: JsValue) -> bool {
        let Some(target) = self.objects.get_mut(object.0) else {
            return false;
        };
        if let Some(property) = target.properties.get_mut(&key) {
            if !property.writable {
                return false;
            }
            property.value = value;
        } else {
            // ECMA-262 §10.4.3.2: a String exotic object's characters and
            // `length` are non-writable own slots, so writing them fails
            // instead of creating a shadowing data property.
            if matches!(target.host, ObjectHost::StringPrimitive(_))
                && (key == "length" || is_canonical_index(&key))
            {
                return false;
            }
            if !target.extensible {
                return false;
            }
            if !is_canonical_index(&key) {
                target.key_order.push(key.clone());
            }
            target
                .properties
                .insert(key, PropertyDescriptor::data(value));
        }
        true
    }

    /// The wrapper object of `node`, created on first use with the prototype of
    /// its DOM `interface` (`"HTMLDivElement"`, `"Text"`, …).
    pub(crate) fn node_wrapper(&mut self, node: NodeId, interface: &str) -> ObjectId {
        if let Some(wrapper) = self.node_wrappers.get(&node) {
            return *wrapper;
        }
        let prototype = self
            .dom_prototypes
            .get(interface)
            .copied()
            .unwrap_or(self.element_prototype);
        let wrapper = self.allocate(JsObject {
            prototype: Some(prototype),
            host: ObjectHost::Node(node),
            ..JsObject::default()
        });
        self.node_wrappers.insert(node, wrapper);
        wrapper
    }

    pub(crate) fn class_list_wrapper(&mut self, node: NodeId) -> ObjectId {
        if let Some(wrapper) = self.class_list_wrappers.get(&node) {
            return *wrapper;
        }
        let wrapper = self.allocate(JsObject {
            prototype: Some(self.object_prototype),
            host: ObjectHost::ClassList(node),
            ..JsObject::default()
        });
        self.class_list_wrappers.insert(node, wrapper);
        wrapper
    }

    pub(crate) fn style_declaration_wrapper(&mut self, node: NodeId) -> ObjectId {
        if let Some(wrapper) = self.style_declaration_wrappers.get(&node) {
            return *wrapper;
        }
        let wrapper = self.allocate(JsObject {
            prototype: Some(self.object_prototype),
            host: ObjectHost::CssStyleDeclaration(node),
            ..JsObject::default()
        });
        self.style_declaration_wrappers.insert(node, wrapper);
        wrapper
    }

    /// Create the cached `element.dataset` `DOMStringMap` wrapper.
    pub(crate) fn dataset_wrapper(&mut self, node: NodeId) -> ObjectId {
        if let Some(wrapper) = self.dataset_wrappers.get(&node) {
            return *wrapper;
        }
        let wrapper = self.allocate(JsObject {
            prototype: Some(self.object_prototype),
            host: ObjectHost::DataSet(node),
            ..JsObject::default()
        });
        self.dataset_wrappers.insert(node, wrapper);
        wrapper
    }

    /// Create a fresh transient wrapper exposing string prototype members.
    pub(crate) fn string_wrapper(&mut self, value: String) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.string_prototype),
            host: ObjectHost::StringPrimitive(value),
            ..JsObject::default()
        })
    }

    /// Create a transient number wrapper exposing Number.prototype members.
    pub(crate) fn number_primitive_wrapper(&mut self, value: f64) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.number_primitive_prototype),
            host: ObjectHost::NumberPrimitive(value),
            ..JsObject::default()
        })
    }

    /// Create a transient boolean wrapper exposing Boolean.prototype members.
    pub(crate) fn boolean_primitive_wrapper(&mut self, value: bool) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.boolean_primitive_prototype),
            host: ObjectHost::BooleanPrimitive(value),
            ..JsObject::default()
        })
    }

    /// Create a fresh `Date` instance carrying epoch milliseconds.
    /// A new Date instance; `prototype` is the one `new.target` supplies, and
    /// the realm's `%Date.prototype%` when that is absent.
    pub(crate) fn date_wrapper(&mut self, ms: f64, prototype: Option<ObjectId>) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(prototype.unwrap_or(self.date_prototype)),
            host: ObjectHost::DateInstance(ms),
            ..JsObject::default()
        })
    }

    /// Mutate a `DateInstance` host in place.
    pub(crate) fn set_host_data_date(&mut self, object: ObjectId, ms: f64) {
        if let Some(JsObject {
            host: ObjectHost::DateInstance(existing),
            ..
        }) = self.objects.get_mut(object.0)
        {
            *existing = ms;
        }
    }

    /// Create the `element.attributes` map wrapper.
    pub(crate) fn named_node_map_wrapper(&mut self, node: NodeId) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.object_prototype),
            host: ObjectHost::NamedNodeMap(node),
            ..JsObject::default()
        })
    }

    /// Create an `Attr` wrapper for `name` on `owner`.
    pub(crate) fn attr_wrapper(&mut self, owner: NodeId, name: String) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.object_prototype),
            host: ObjectHost::Attr { owner, name },
            ..JsObject::default()
        })
    }

    /// Create a fresh `RegExp` instance backed by compiled record `index`.
    pub(crate) fn regexp_wrapper(&mut self, index: usize) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.regexp_prototype),
            host: ObjectHost::RegExp(index),
            ..JsObject::default()
        })
    }

    pub(crate) fn bound_function(
        &mut self,
        function: NativeFunction,
        receiver: ObjectId,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::BoundFunction { function, receiver },
            ..JsObject::default()
        })
    }

    pub(crate) fn bound_callable(
        &mut self,
        target: ObjectId,
        receiver: JsValue,
        arguments: Vec<JsValue>,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::BoundCallable {
                target,
                receiver,
                arguments,
            },
            ..JsObject::default()
        })
    }

    pub(crate) fn arrow_function(
        &mut self,
        function: usize,
        name: &str,
        length: usize,
    ) -> ObjectId {
        let object = self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::ArrowFunction(function),
            ..JsObject::default()
        });
        self.install_function_metadata(object, name, length);
        object
    }

    /// A user function object of the given kind. An async function has no
    /// `prototype` property. A generator's `prototype` inherits from
    /// `%GeneratorPrototype%` (or `%AsyncGeneratorPrototype%`) and has no
    /// `constructor` (ECMA-262 27.3.4.2, 27.6.1.1, 27.7.4), while an ordinary
    /// function's `prototype` has the `constructor` back-link. An async generator
    /// function inherits from `%AsyncGeneratorFunction.prototype%`.
    pub(crate) fn user_function(
        &mut self,
        function: usize,
        name: &str,
        length: usize,
        kind: FunctionKind,
    ) -> ObjectId {
        let prototype = match kind {
            FunctionKind::Async => None,
            FunctionKind::Normal => Some(self.create_ordinary_object()),
            FunctionKind::Generator => Some(self.allocate(JsObject {
                prototype: Some(self.generator_prototype),
                ..JsObject::default()
            })),
            FunctionKind::AsyncGenerator => Some(self.allocate(JsObject {
                prototype: Some(self.async_generator_prototype),
                ..JsObject::default()
            })),
        };
        let function_prototype = if kind == FunctionKind::AsyncGenerator {
            self.async_generator_function_prototype
        } else {
            self.function_prototype
        };
        let callable = self.allocate(JsObject {
            prototype: Some(function_prototype),
            host: ObjectHost::UserFunction(function),
            ..JsObject::default()
        });
        if let Some(prototype) = prototype {
            self.objects[callable.0].properties.insert(
                "prototype".to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value: JsValue::Object(prototype),
                    writable: true,
                    enumerable: false,
                    configurable: false,
                },
            );
            if kind == FunctionKind::Normal {
                self.objects[prototype.0].properties.insert(
                    "constructor".to_owned(),
                    PropertyDescriptor {
                        getter: None,
                        setter: None,
                        value: JsValue::Object(callable),
                        writable: true,
                        enumerable: false,
                        configurable: true,
                    },
                );
            }
        }
        self.install_function_metadata(callable, name, length);
        // A user function's own keys come first in the spec order: `length`,
        // `name`, then `prototype`; the keys it gains later follow them.
        let target = &mut self.objects[callable.0];
        target
            .key_order
            .extend(["length", "name"].map(str::to_owned));
        if prototype.is_some() {
            target.key_order.push("prototype".to_owned());
        }
        callable
    }

    /// Install the spec `name` and `length` own data properties shared by
    /// every callable flavor: non-writable, non-enumerable, configurable.
    pub(crate) fn install_function_metadata(
        &mut self,
        object: ObjectId,
        name: &str,
        length: usize,
    ) {
        #[allow(
            clippy::cast_precision_loss,
            reason = "parameter counts stay far below any precision boundary"
        )]
        let length = length as f64;
        for (property, value) in [
            ("name", JsValue::String(name.to_owned())),
            ("length", JsValue::Number(length)),
        ] {
            self.objects[object.0].properties.insert(
                property.to_owned(),
                PropertyDescriptor {
                    getter: None,
                    setter: None,
                    value,
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            );
        }
    }

    /// A standalone native function object (used to build callables that
    /// capture per-call state through `bound_callable`).
    #[must_use]
    pub(crate) const fn object_prototype_id(&self) -> ObjectId {
        self.object_prototype
    }

    /// The intrinsic prototypes `Object.prototype.__proto__` reports for a
    /// primitive wrapper (Annex B.2.2.1), keyed by the host the wrapper carries.
    pub(crate) fn intrinsic_prototype_for_host(&self, object: ObjectId) -> Option<ObjectId> {
        match self.objects.get(object.0)?.host {
            ObjectHost::StringPrimitive(_) => Some(self.string_prototype),
            ObjectHost::NumberPrimitive(_) => Some(self.number_primitive_prototype),
            ObjectHost::BooleanPrimitive(_) => Some(self.boolean_primitive_prototype),
            ObjectHost::SymbolInstance(_) => Some(self.symbol_prototype),
            ObjectHost::BigIntPrimitive(_) => Some(self.bigint_prototype),
            _ => None,
        }
    }

    pub(crate) fn native_object(&mut self, function: NativeFunction) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::NativeFunction(function),
            ..JsObject::default()
        })
    }

    pub(crate) fn promise(&mut self, promise: usize) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.promise_prototype),
            host: ObjectHost::Promise(promise),
            ..JsObject::default()
        })
    }

    /// A new generator object whose behaviour lives in coroutine `coroutine`.
    pub(crate) fn generator_object(
        &mut self,
        coroutine: usize,
        prototype: Option<ObjectId>,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(prototype.unwrap_or(self.generator_prototype)),
            host: ObjectHost::Generator(coroutine),
            ..JsObject::default()
        })
    }

    /// A new async generator object whose behaviour lives in coroutine `coroutine`.
    pub(crate) fn async_generator_object(
        &mut self,
        coroutine: usize,
        prototype: Option<ObjectId>,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(prototype.unwrap_or(self.async_generator_prototype)),
            host: ObjectHost::AsyncGenerator(coroutine),
            ..JsObject::default()
        })
    }

    /// `CreateAsyncFromSyncIterator`'s wrapper object (ECMA-262 27.1.4.1).
    pub(crate) fn async_from_sync_iterator(
        &mut self,
        iterator: ObjectId,
        next: ObjectId,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.async_from_sync_iterator_prototype),
            host: ObjectHost::AsyncFromSyncIterator { iterator, next },
            ..JsObject::default()
        })
    }

    pub(crate) fn async_from_sync_value(&mut self, done: bool) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::AsyncFromSyncValue { done },
            ..JsObject::default()
        })
    }

    pub(crate) fn async_from_sync_close(&mut self, iterator: ObjectId) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::AsyncFromSyncClose { iterator },
            ..JsObject::default()
        })
    }

    /// A `DisposableStack` or `AsyncDisposableStack` instance with `state`.
    pub(crate) fn disposable_stack(
        &mut self,
        prototype: ObjectId,
        state: DisposeStackState,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(prototype),
            host: ObjectHost::DisposableStack(state),
            ..JsObject::default()
        })
    }

    /// The `onFulfilled` (`rejected == false`) or `onRejected` callback of an
    /// `await` inside `disposeAsync` on `stack`.
    pub(crate) fn dispose_resume_function(&mut self, stack: ObjectId, rejected: bool) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::DisposeAwaitResume { stack, rejected },
            ..JsObject::default()
        })
    }

    /// `%DisposableStack.prototype%` or `%AsyncDisposableStack.prototype%`.
    pub(crate) const fn intrinsic_disposable_stack_prototype(
        &self,
        kind: DisposeStackKind,
    ) -> ObjectId {
        match kind {
            DisposeStackKind::Sync => self.disposable_stack_prototype,
            DisposeStackKind::Async => self.async_disposable_stack_prototype,
        }
    }

    /// `%SuppressedError.prototype%`.
    pub(crate) const fn intrinsic_suppressed_error_prototype(&self) -> ObjectId {
        self.suppressed_error_prototype
    }

    pub(crate) fn async_resume(&mut self, coroutine: usize, rejected: bool) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::AsyncResume {
                coroutine,
                rejected,
            },
            ..JsObject::default()
        })
    }

    /// A resolving function of `promise` (ECMA-262 27.2.1.3): a built-in
    /// function of length 1 and empty name, not a constructor.
    pub(crate) fn promise_settler(
        &mut self,
        promise: usize,
        fulfilled: bool,
        pair: usize,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.function_prototype),
            host: ObjectHost::PromiseSettler {
                promise,
                fulfilled,
                pair,
            },
            properties: Self::function_metadata("", 1.0),
            ..JsObject::default()
        })
    }

    /// `%Promise%`, the intrinsic constructor.
    pub(crate) const fn promise_constructor(&self) -> ObjectId {
        self.promise_constructor
    }

    pub(crate) fn collection(
        &mut self,
        kind: CollectionKind,
        prototype: Option<ObjectId>,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype,
            host: ObjectHost::Collection {
                kind,
                entries: Vec::new(),
            },
            ..JsObject::default()
        })
    }

    /// Create one typed-array view object. Its `length`, `byteLength` and
    /// `byteOffset` are accessors on `%TypedArray%.prototype` that read the host
    /// state, so the instance carries no own properties for them; indexed
    /// elements are synthesized from the shared buffer on read.
    pub(crate) fn typed_array(
        &mut self,
        kind: TypedArrayKind,
        buffer: TypedBuffer,
        start: usize,
        length: Option<usize>,
        prototype: Option<ObjectId>,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype,
            host: ObjectHost::TypedArray {
                kind,
                buffer,
                start,
                length,
            },
            ..JsObject::default()
        })
    }

    /// A `%RegExpStringIterator%` over `input`, stepping `matcher` (ECMA-262
    /// 22.2.9.1 `CreateRegExpStringIterator`).
    pub(crate) fn regexp_string_iterator(
        &mut self,
        matcher: ObjectId,
        input: String,
        global: bool,
        unicode: bool,
    ) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.regexp_string_iterator_prototype),
            host: ObjectHost::RegExpStringIterator {
                matcher,
                input,
                global,
                unicode,
                done: false,
            },
            ..JsObject::default()
        })
    }

    /// A live iterator over `target`'s indexed elements, yielding `view`.
    pub(crate) fn array_iterator(&mut self, target: ObjectId, view: ArrayView) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.iterator_prototype),
            host: ObjectHost::ArrayIterator {
                target: Some(target),
                index: 0,
                view,
            },
            ..JsObject::default()
        })
    }

    pub(crate) fn collection_iterator(&mut self, values: Vec<JsValue>) -> ObjectId {
        self.allocate(JsObject {
            prototype: Some(self.iterator_prototype),
            host: ObjectHost::CollectionIterator { values, index: 0 },
            ..JsObject::default()
        })
    }

    /// Allocate a lazy iterator-helper state machine object. `Iterator.from`
    /// wrappers take `%WrapForValidIteratorPrototype%`, every other kind takes
    /// `%IteratorHelperPrototype%`.
    pub(crate) fn iterator_helper(
        &mut self,
        kind: IteratorHelperKind,
        source: Option<ObjectId>,
        source_next: Option<ObjectId>,
        callback: Option<ObjectId>,
        counter: u64,
    ) -> ObjectId {
        let prototype = if kind == IteratorHelperKind::Wrap {
            self.iterator_wrap_prototype
        } else {
            self.iterator_helper_prototype
        };
        self.allocate(JsObject {
            prototype: Some(prototype),
            host: ObjectHost::IteratorHelper {
                kind,
                source,
                source_next,
                callback,
                inner: None,
                inner_next: None,
                counter,
                buffer: Vec::new(),
                done: false,
                running: false,
                started: false,
            },
            ..JsObject::default()
        })
    }

    /// `%Iterator.prototype%`, the intrinsic that `Iterator.from` tests against.
    pub(crate) const fn iterator_prototype_id(&self) -> ObjectId {
        self.iterator_prototype
    }

    fn allocate(&mut self, object: JsObject) -> ObjectId {
        let id = ObjectId(self.objects.len());
        self.objects.push(object);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::{JsValue, PropertyDescriptor, Realm};
    use render_dom::Dom;
    use url::Url;

    /// Every name in [`Realm::builtin_arity`] resolves to a callable the realm
    /// actually installed.
    ///
    /// The arity table reads, to anyone scanning this file, like a list of the
    /// built-ins this engine has. It was not one: it carried forty names nothing
    /// installed, including `String.prototype.codePointAt` and `Promise.all`,
    /// which is how a later agent concluded `codePointAt` existed. The table has
    /// no runtime effect for a name the engine does not install - nothing ever
    /// looks it up - so the defect was invisible to every test and visible only
    /// to a reader. This test is what makes it visible to a machine instead.
    ///
    /// The direction matters. Asserting "these forty are absent" would pin the
    /// absence and block the feature; asserting "everything listed is present"
    /// passes the moment the feature lands, provided the arity entry is added
    /// with it, and fails today for a claim rather than for a gap.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the claimed-name list is the assertion; splitting it out would hide it"
    )]
    fn the_arity_table_only_names_the_engine_installs() {
        let dom = Dom::new();
        let realm = Realm::bootstrap(
            dom.document(),
            &Url::parse("about:blank").expect("test URL"),
        );
        let mut installed = std::collections::BTreeSet::new();
        for object in realm.objects() {
            for (key, descriptor) in &object.properties {
                // A name is "installed" when some callable in the realm answers
                // to it. Checking the descriptor's own value is not enough: a
                // method on a prototype is the normal case, and `Function.prototype`
                // methods are one hop further.
                if descriptor.is_accessor() {
                    continue;
                }
                if let JsValue::Object(target) = descriptor.value
                    && let Some(target) = realm.object(target)
                    && target.host.is_callable()
                {
                    installed.insert(key.clone());
                }
            }
        }
        let mut claims = std::collections::BTreeSet::new();
        for name in [
            // The arity table's own vocabulary, kept as a literal list here
            // rather than by re-parsing the match: a test that reads the
            // implementation cannot fail when the implementation is wrong.
            "push",
            "map",
            "filter",
            "forEach",
            "some",
            "every",
            "find",
            "findIndex",
            "findLast",
            "findLastIndex",
            "includes",
            "indexOf",
            "lastIndexOf",
            "charAt",
            "charCodeAt",
            "codePointAt",
            "at",
            "repeat",
            "resolve",
            "reject",
            "catch",
            "finally",
            "get",
            "has",
            "add",
            "bind",
            "isArray",
            "from",
            "getOwnPropertyDescriptors",
            "getOwnPropertySymbols",
            "getPrototypeOf",
            "hasOwnProperty",
            "isPrototypeOf",
            "propertyIsEnumerable",
            "parseFloat",
            "isNaN",
            "isFinite",
            "parse",
            "exec",
            "test",
            "toFixed",
            "toPrecision",
            "match",
            "search",
            "localeCompare",
            "startsWith",
            "endsWith",
            "sort",
            "reduce",
            "reduceRight",
            "fill",
            "flatMap",
            "freeze",
            "seal",
            "preventExtensions",
            "isFrozen",
            "isSealed",
            "isExtensible",
            "getOwnPropertyNames",
            "setPrototypeOf",
            "trim",
            "trimStart",
            "trimEnd",
            "then",
            "set",
            "apply",
            "create",
            "defineProperties",
            "replace",
            "replaceAll",
            "slice",
            "substring",
            "substr",
            "splice",
            "padStart",
            "padEnd",
            "parseInt",
            "assign",
            "getOwnPropertyDescriptor",
            "defineProperty",
            "construct",
            "toString",
            "valueOf",
            "toISOString",
            "toJSON",
            "toUTCString",
            "toDateString",
            "now",
            "getTime",
            "getFullYear",
            "getUTCFullYear",
            "getMonth",
            "getUTCMonth",
            "getDate",
            "getUTCDate",
            "getDay",
            "getUTCDay",
            "getHours",
            "getUTCHours",
            "getMinutes",
            "getUTCMinutes",
            "getSeconds",
            "getUTCSeconds",
            "getMilliseconds",
            "getUTCMilliseconds",
            "getTimezoneOffset",
            "pop",
            "shift",
            "clear",
            "next",
            "return",
            "random",
            "flat",
            "keys",
            "values",
            "entries",
            "toArray",
            "toLowerCase",
            "toUpperCase",
            "Object",
            "Function",
            "Array",
            "String",
            "Number",
            "Boolean",
            "Error",
            "TypeError",
            "RangeError",
            "SyntaxError",
            "ReferenceError",
            "EvalError",
            "URIError",
            "Promise",
            "ArrayBuffer",
            "DataView",
            "Symbol",
            "Map",
            "Set",
            "WeakMap",
            "WeakSet",
            "Iterator",
            "Uint8Array",
            "Uint8ClampedArray",
            "Int8Array",
            "Uint16Array",
            "Int16Array",
            "Uint32Array",
            "Int32Array",
            "Float32Array",
            "Float64Array",
            "Date",
            "UTC",
            "RegExp",
        ] {
            claims.insert(name.to_owned());
        }
        let unbacked: Vec<&String> = claims
            .iter()
            .filter(|name| !installed.contains(*name))
            .collect();
        assert!(
            unbacked.is_empty(),
            "the arity table claims {} name(s) the engine does not install; \
             either implement the member or drop the entry, because a listed \
             name that does not exist reads to the next reader as a capability \
             that does.",
            unbacked.len()
        );
        assert!(unbacked.is_empty(), "not installed: {unbacked:?}");
    }

    /// Every installed constructor reports `typeof "function"`.
    ///
    /// `ArrayBuffer`, `DataView`, `TextEncoder` and `TextDecoder` were all
    /// installed and all reported `"object"`, because the callable-host set was
    /// written down twice and both copies had the same four variants missing. The
    /// shape of the assertion is the point: a global that owns a `prototype` and
    /// is not a function is what a broken callable set looks like from script, and
    /// `typeof X === "function"` is the gate every feature-detection idiom in a
    /// production bundle passes through.
    #[test]
    fn an_installed_constructor_reports_a_function_typeof() {
        let dom = Dom::new();
        let realm = Realm::bootstrap(
            dom.document(),
            &Url::parse("about:blank").expect("test URL"),
        );
        let global = realm.global_object();
        let mut wrong = Vec::new();
        let global_object = realm.object(global).expect("the global object exists");
        for (key, descriptor) in &global_object.properties {
            let JsValue::Object(value) = descriptor.value else {
                continue;
            };
            // A global that owns an object-valued `prototype` is a constructor,
            // and a constructor whose `typeof` is not `"function"` breaks every
            // `typeof X === "function"` feature probe that guards it.
            let Some(host) = realm.host(value) else {
                continue;
            };
            let looks_like_a_constructor = realm
                .get_property(value, "prototype")
                .is_some_and(|property| matches!(property, JsValue::Object(_)));
            if looks_like_a_constructor && !host.is_callable() {
                wrong.push(key.clone());
            }
        }
        assert!(
            wrong.is_empty(),
            "these globals own a prototype but do not report `typeof \"function\"`: {wrong:?}"
        );
    }

    #[test]
    fn ordinary_properties_follow_the_prototype_chain() {
        let dom = Dom::new();
        let mut realm = Realm::bootstrap(
            dom.document(),
            &Url::parse("about:blank").expect("test URL"),
        );
        let prototype = realm.create_object(None);
        assert!(realm.define_property(
            prototype,
            "answer",
            PropertyDescriptor::data(JsValue::Number(42.0)),
        ));
        let object = realm.create_object(Some(prototype));
        assert_eq!(
            realm.get_property(object, "answer"),
            Some(JsValue::Number(42.0))
        );
    }

    #[test]
    fn global_numeric_constants_are_immutable_and_non_enumerable() {
        let dom = Dom::new();
        let mut realm = Realm::bootstrap(
            dom.document(),
            &Url::parse("about:blank").expect("test URL"),
        );
        let global = realm.global_object();

        assert!(matches!(realm.global("NaN"), Some(JsValue::Number(value)) if value.is_nan()));
        assert_eq!(
            realm.global("Infinity"),
            Some(JsValue::Number(f64::INFINITY))
        );
        assert_eq!(realm.global("undefined"), Some(JsValue::Undefined));
        for name in ["NaN", "Infinity", "undefined"] {
            let descriptor = realm
                .own_property(global, name)
                .expect("global constant should have an own descriptor");
            assert!(!descriptor.writable);
            assert!(!descriptor.enumerable);
            assert!(!descriptor.configurable);
            assert!(!realm.set_global(name.to_owned(), JsValue::Number(1.0)));
            assert!(!realm.delete_property(global, name));
        }
    }
}
