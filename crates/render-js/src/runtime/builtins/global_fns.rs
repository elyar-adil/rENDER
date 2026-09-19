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
    clippy::too_many_lines,
    clippy::wrong_self_convention
)]

use render_dom::Dom;
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::runtime::convert::to_number;
use crate::runtime::types::ConsoleLevel;
use crate::runtime::types::ConsoleMessage;
use crate::runtime::types::JsMicrotask;
use crate::runtime::types::MAX_BUFFERED_CONSOLE_MESSAGES;
use crate::runtime::types::TimerKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use std::fmt::Write as _;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_residual_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            // Fetch-domain functions are intercepted at the dispatch head in
            // `fetch.rs`; these arms exist so a new variant stays a compile
            // error here instead of a silent runtime gap.
            NativeFunction::GlobalFetch
            | NativeFunction::ResponseText
            | NativeFunction::ResponseJson
            | NativeFunction::ResponseHeadersGet
            | NativeFunction::XhrOpen
            | NativeFunction::XhrSetRequestHeader
            | NativeFunction::XhrSend
            | NativeFunction::XhrGetResponseHeader => {
                self.dispatch_fetch_native(dom, function, receiver, arguments)
            }
            // Video-domain functions are intercepted right after the DOM
            // dispatch in `video.rs`; route to it for the same reason.
            NativeFunction::VideoPlay
            | NativeFunction::VideoPause
            | NativeFunction::VideoLoad
            | NativeFunction::VideoCanPlayType => {
                self.dispatch_video_native(dom, function, receiver, arguments)
            }
            NativeFunction::AddEventListener => self.add_event_listener(receiver, arguments),
            NativeFunction::RemoveEventListener => self.remove_event_listener(receiver, arguments),
            NativeFunction::DispatchEvent => self.dispatch_event(dom, receiver, arguments),
            NativeFunction::LocationToString => match self.realm.host(receiver) {
                Some(ObjectHost::Location(url)) => Ok(JsValue::String(url.to_string())),
                _ => Err(JsError::type_error("incompatible Location method receiver")),
            },
            NativeFunction::LocationAssign | NativeFunction::LocationReplace => {
                self.request_location_navigation(receiver, arguments, function)
            }
            NativeFunction::ConsoleDebug
            | NativeFunction::ConsoleError
            | NativeFunction::ConsoleInfo
            | NativeFunction::ConsoleLog
            | NativeFunction::ConsoleWarn => self.console_write(function, arguments),
            NativeFunction::RequestAnimationFrame => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "requestAnimationFrame")?,
                    &self.realm,
                )?;
                Ok(JsValue::Number(self.register_timer_entry(
                    callback,
                    0.0,
                    TimerKind::AnimationFrame,
                )))
            }
            NativeFunction::ClearTimeout | NativeFunction::ClearInterval => {
                Ok(self.cancel_timer(arguments))
            }
            NativeFunction::CancelAnimationFrame => Ok(self.cancel_timer(arguments)),
            NativeFunction::GetBoundingClientRect => {
                let node = self.require_node(receiver)?;
                Ok(self.element_rect_value(node))
            }
            NativeFunction::SymbolDescription => match self.realm.host(receiver) {
                Some(ObjectHost::SymbolInstance(symbol)) => Ok(symbol
                    .description()
                    .map_or(JsValue::Undefined, |text| JsValue::String(text.to_owned()))),
                _ => Err(JsError::type_error(
                    "Symbol.prototype.description requires that 'this' be a Symbol",
                )),
            },
            NativeFunction::PromiseFinally
            | NativeFunction::PromiseFinallyPass
            | NativeFunction::PromiseFinallyReject => {
                self.dispatch_promise_native(dom, function, receiver, arguments)
            }
            NativeFunction::SymbolFor => {
                let key = required_argument(arguments, 0, "Symbol.for")?.to_js_string();
                Ok(JsValue::Symbol(self.symbol_for(key)))
            }
            NativeFunction::SymbolKeyFor => {
                let value = required_argument(arguments, 0, "Symbol.keyFor")?;
                let result = match value {
                    JsValue::Symbol(symbol) => self
                        .global_symbol_registry
                        .iter()
                        .find(|(_, id)| **id == symbol.id())
                        .map(|(key, _)| JsValue::String(key.clone()))
                        .unwrap_or(JsValue::Undefined),
                    _ => JsValue::Undefined,
                };
                Ok(result)
            }
            NativeFunction::ObjectDefineGetter => {
                self.object_define_accessor(receiver, arguments, true)
            }
            NativeFunction::ObjectPreventExtensions => {
                let object = self.integrity_target(arguments, "preventExtensions")?;
                self.realm.prevent_extensions(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectSeal => {
                let object = self.integrity_target(arguments, "seal")?;
                self.realm.seal_object(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectFreeze => {
                let object = self.integrity_target(arguments, "freeze")?;
                self.realm.freeze_object(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectIsExtensible => {
                let object = self.integrity_target(arguments, "isExtensible")?;
                Ok(JsValue::Boolean(self.realm.is_extensible(object)))
            }
            NativeFunction::ObjectIsSealed => {
                let object = self.integrity_target(arguments, "isSealed")?;
                Ok(JsValue::Boolean(self.realm.is_sealed(object)))
            }
            NativeFunction::ObjectIsFrozen => {
                let object = self.integrity_target(arguments, "isFrozen")?;
                Ok(JsValue::Boolean(self.realm.is_frozen(object)))
            }
            NativeFunction::ObjectDefineSetter => {
                self.object_define_accessor(receiver, arguments, false)
            }
            NativeFunction::ObjectLookupGetter => {
                self.object_lookup_accessor(receiver, arguments, true)
            }
            NativeFunction::ObjectLookupSetter => {
                self.object_lookup_accessor(receiver, arguments, false)
            }
            NativeFunction::StrCharAt => self.string_char_at(receiver, arguments),
            NativeFunction::StrCharCodeAt => self.string_char_code_at(receiver, arguments),
            NativeFunction::StrIndexOf => self.string_index_of(receiver, arguments, false),
            NativeFunction::StrLastIndexOf => self.string_index_of(receiver, arguments, true),
            NativeFunction::StrIncludes => self.string_includes(receiver, arguments),
            NativeFunction::StrStartsWith => {
                self.string_starts_or_ends_with(receiver, arguments, true)
            }
            NativeFunction::StrEndsWith => {
                self.string_starts_or_ends_with(receiver, arguments, false)
            }
            NativeFunction::StrSlice => self.string_slice(receiver, arguments),
            NativeFunction::StrSubstring => self.string_substring(receiver, arguments),
            NativeFunction::StrToLowerCase => self.string_to_case(receiver, arguments, false),
            NativeFunction::StrToUpperCase => self.string_to_case(receiver, arguments, true),
            NativeFunction::StrTrim => self.string_trim(receiver),
            NativeFunction::StrSplit => self.string_split(receiver, arguments),
            NativeFunction::StrReplace => self.string_replace(dom, receiver, arguments),
            NativeFunction::StrMatch => self.string_match(receiver, arguments),
            NativeFunction::StrSearch => self.string_search(receiver, arguments),
            NativeFunction::StrConcat => self.string_concat(receiver, arguments),
            NativeFunction::StrToString => {
                Ok(JsValue::String(self.require_string_object(receiver)?))
            }
            NativeFunction::StrForEach => {
                let text = self.require_string_receiver(receiver)?;
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "String.forEach")?,
                    &self.realm,
                )?;
                for (index, character) in text.chars().enumerate() {
                    #[allow(clippy::cast_precision_loss)]
                    let index = JsValue::Number(index as f64);
                    self.call(
                        dom,
                        callback,
                        &[
                            JsValue::String(character.to_string()),
                            index,
                            JsValue::Object(receiver),
                        ],
                    )?;
                }
                Ok(JsValue::Undefined)
            }
            NativeFunction::StrPush => Ok(JsValue::Number(
                self.require_string_receiver(receiver)?.chars().count() as f64,
            )),
            NativeFunction::QueueMicrotask => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "queueMicrotask")?,
                    &self.realm,
                )?;
                self.pending_microtasks
                    .push(JsMicrotask::Callback(callback));
                Ok(JsValue::Undefined)
            }
            NativeFunction::GlobalEvalStub => {
                // Indirect eval is not supported; return the argument
                // unchanged so JSONP-style `eval(data)` patterns don't crash.
                Ok(arguments.first().cloned().unwrap_or(JsValue::Undefined))
            }
            NativeFunction::GlobalImport => {
                // Module graph fetching belongs to the browser coordinator.
                // Keep dynamic import thenable so feature detection and
                // optional chunks do not turn into a fatal ReferenceError.
                let (promise, result) = self.create_promise()?;
                let namespace = self.realm.create_ordinary_object();
                self.resolve_promise(promise, &JsValue::Object(namespace));
                Ok(result)
            }
            NativeFunction::GlobalNoop => Ok(JsValue::Undefined),
            NativeFunction::CssSupports => Ok(JsValue::Boolean(true)),
            NativeFunction::GlobalEscape => {
                let text = required_argument(arguments, 0, "escape")?.to_js_string();
                let mut output = String::with_capacity(text.len());
                for character in text.chars() {
                    let code = character as u32;
                    if code < 0x80
                        && (character.is_ascii_alphanumeric()
                            || matches!(
                                character,
                                '@' | '*' | '_' | '+' | '-' | '.' | '/' | '(' | ')'
                            ))
                    {
                        output.push(character);
                    } else if code < 0x100 {
                        let _ = write!(output, "%{code:02X}");
                    } else {
                        let _ = write!(output, "%u{code:04X}");
                    }
                }
                Ok(JsValue::String(output))
            }
            NativeFunction::GlobalUnescape => {
                let text = required_argument(arguments, 0, "unescape")?.to_js_string();
                match Self::percent_decode(&text) {
                    Some(decoded) => Ok(JsValue::String(decoded)),
                    None => Ok(JsValue::String(text)),
                }
            }
            NativeFunction::GlobalParseInt => {
                let text = required_argument(arguments, 0, "parseInt")?.to_js_string();
                let trimmed = text.trim_start();
                let (radix, digits) = if let Some(rest) = trimmed.strip_prefix("0x") {
                    (16u32, rest)
                } else if let Some(rest) = trimmed.strip_prefix("0X") {
                    (16, rest)
                } else {
                    (10, trimmed)
                };
                let end = digits
                    .chars()
                    .position(|c| c.to_digit(radix).is_none())
                    .unwrap_or(digits.len());
                match i64::from_str_radix(&digits[..end], radix) {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "parseInt results stay within binary64 precision"
                    )]
                    Ok(value) => Ok(JsValue::Number(value as f64)),
                    Err(_) => Ok(JsValue::Number(f64::NAN)),
                }
            }
            NativeFunction::GlobalParseFloat => {
                let text = required_argument(arguments, 0, "parseFloat")?.to_js_string();
                let trimmed = text.trim_start();
                let end = trimmed
                    .find(|c: char| {
                        !(c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
                    })
                    .unwrap_or(trimmed.len());
                trimmed[..end].parse::<f64>().map_or_else(
                    |_| Ok(JsValue::Number(f64::NAN)),
                    |value| Ok(JsValue::Number(value)),
                )
            }
            NativeFunction::GlobalIsNaN => Ok(JsValue::Boolean(
                to_number(required_argument(arguments, 0, "isNaN")?)?.is_nan(),
            )),
            NativeFunction::GlobalIsFinite => Ok(JsValue::Boolean(
                to_number(required_argument(arguments, 0, "isFinite")?)?.is_finite(),
            )),
            NativeFunction::GlobalEncodeURI | NativeFunction::GlobalEncodeURIComponent => {
                let text = required_argument(arguments, 0, "encodeURI")?.to_js_string();
                let component = function == NativeFunction::GlobalEncodeURIComponent;
                Ok(JsValue::String(Self::percent_encode(&text, !component)))
            }
            NativeFunction::GlobalDecodeURI | NativeFunction::GlobalDecodeURIComponent => {
                let text = required_argument(arguments, 0, "decodeURI")?.to_js_string();
                match Self::percent_decode(&text) {
                    Some(decoded) => Ok(JsValue::String(decoded)),
                    None => Err(JsError::dom("malformed URI sequence")),
                }
            }
            NativeFunction::PerformanceNow => Ok(JsValue::Number(Self::monotonic_now_ms())),
            NativeFunction::FunctionPrototype
            | NativeFunction::FunctionCall
            | NativeFunction::FunctionApply
            | NativeFunction::FunctionBind => {
                unreachable!("Function prototype methods use arbitrary receivers")
            }
            NativeFunction::AppendChild => {
                self.dispatch_dom_native(dom, NativeFunction::AppendChild, receiver, arguments)
            }
            NativeFunction::ArrayConcat => {
                self.dispatch_array_native(dom, NativeFunction::ArrayConcat, receiver, arguments)
            }
            NativeFunction::ArrayEvery => {
                self.dispatch_array_native(dom, NativeFunction::ArrayEvery, receiver, arguments)
            }
            NativeFunction::ArrayFilter => {
                self.dispatch_array_native(dom, NativeFunction::ArrayFilter, receiver, arguments)
            }
            NativeFunction::ArrayFind => {
                self.dispatch_array_native(dom, NativeFunction::ArrayFind, receiver, arguments)
            }
            NativeFunction::ArrayFindIndex => {
                self.dispatch_array_native(dom, NativeFunction::ArrayFindIndex, receiver, arguments)
            }
            NativeFunction::ArrayForEach => {
                self.dispatch_array_native(dom, NativeFunction::ArrayForEach, receiver, arguments)
            }
            NativeFunction::ArrayFrom => {
                self.dispatch_array_native(dom, NativeFunction::ArrayFrom, receiver, arguments)
            }
            NativeFunction::ArrayIncludes => {
                self.dispatch_array_native(dom, NativeFunction::ArrayIncludes, receiver, arguments)
            }
            NativeFunction::ArrayIndexOf => {
                self.dispatch_array_native(dom, NativeFunction::ArrayIndexOf, receiver, arguments)
            }
            NativeFunction::ArrayIsArray => {
                self.dispatch_array_native(dom, NativeFunction::ArrayIsArray, receiver, arguments)
            }
            NativeFunction::ArrayJoin => {
                self.dispatch_array_native(dom, NativeFunction::ArrayJoin, receiver, arguments)
            }
            NativeFunction::ArrayMap => {
                self.dispatch_array_native(dom, NativeFunction::ArrayMap, receiver, arguments)
            }
            NativeFunction::ArrayPop => {
                self.dispatch_array_native(dom, NativeFunction::ArrayPop, receiver, arguments)
            }
            NativeFunction::ArrayPrototypeToString => self.dispatch_array_native(
                dom,
                NativeFunction::ArrayPrototypeToString,
                receiver,
                arguments,
            ),
            NativeFunction::ArrayPush => {
                self.dispatch_array_native(dom, NativeFunction::ArrayPush, receiver, arguments)
            }
            NativeFunction::ArrayReduce => {
                self.dispatch_array_native(dom, NativeFunction::ArrayReduce, receiver, arguments)
            }
            NativeFunction::ArrayReverse => {
                self.dispatch_array_native(dom, NativeFunction::ArrayReverse, receiver, arguments)
            }
            NativeFunction::ArrayShift => {
                self.dispatch_array_native(dom, NativeFunction::ArrayShift, receiver, arguments)
            }
            NativeFunction::ArraySlice => {
                self.dispatch_array_native(dom, NativeFunction::ArraySlice, receiver, arguments)
            }
            NativeFunction::ArraySome => {
                self.dispatch_array_native(dom, NativeFunction::ArraySome, receiver, arguments)
            }
            NativeFunction::ArraySort => {
                self.dispatch_array_native(dom, NativeFunction::ArraySort, receiver, arguments)
            }
            NativeFunction::ArraySplice => {
                self.dispatch_array_native(dom, NativeFunction::ArraySplice, receiver, arguments)
            }
            NativeFunction::ArrayUnshift => {
                self.dispatch_array_native(dom, NativeFunction::ArrayUnshift, receiver, arguments)
            }
            NativeFunction::AttrGetName => {
                self.dispatch_dom_native(dom, NativeFunction::AttrGetName, receiver, arguments)
            }
            NativeFunction::AttrGetValue => {
                self.dispatch_dom_native(dom, NativeFunction::AttrGetValue, receiver, arguments)
            }
            NativeFunction::BoolToString => {
                self.dispatch_object_native(dom, NativeFunction::BoolToString, receiver, arguments)
            }
            NativeFunction::BoolValueOf => {
                self.dispatch_object_native(dom, NativeFunction::BoolValueOf, receiver, arguments)
            }
            NativeFunction::ClassListAdd => {
                self.dispatch_dom_native(dom, NativeFunction::ClassListAdd, receiver, arguments)
            }
            NativeFunction::ClassListContains => self.dispatch_dom_native(
                dom,
                NativeFunction::ClassListContains,
                receiver,
                arguments,
            ),
            NativeFunction::ClassListItem => {
                self.dispatch_dom_native(dom, NativeFunction::ClassListItem, receiver, arguments)
            }
            NativeFunction::ClassListRemove => {
                self.dispatch_dom_native(dom, NativeFunction::ClassListRemove, receiver, arguments)
            }
            NativeFunction::ClassListToString => self.dispatch_dom_native(
                dom,
                NativeFunction::ClassListToString,
                receiver,
                arguments,
            ),
            NativeFunction::ClassListToggle => {
                self.dispatch_dom_native(dom, NativeFunction::ClassListToggle, receiver, arguments)
            }
            NativeFunction::Click => {
                self.dispatch_dom_native(dom, NativeFunction::Click, receiver, arguments)
            }
            NativeFunction::CloneNode => {
                self.dispatch_dom_native(dom, NativeFunction::CloneNode, receiver, arguments)
            }
            NativeFunction::CollectionAdd => self.dispatch_collections_native(
                dom,
                NativeFunction::CollectionAdd,
                receiver,
                arguments,
            ),
            NativeFunction::CollectionClear => self.dispatch_collections_native(
                dom,
                NativeFunction::CollectionClear,
                receiver,
                arguments,
            ),
            NativeFunction::CollectionDelete => self.dispatch_collections_native(
                dom,
                NativeFunction::CollectionDelete,
                receiver,
                arguments,
            ),
            NativeFunction::CollectionEntries => self.dispatch_collections_native(
                dom,
                NativeFunction::CollectionEntries,
                receiver,
                arguments,
            ),
            NativeFunction::CollectionForEach => self.dispatch_collections_native(
                dom,
                NativeFunction::CollectionForEach,
                receiver,
                arguments,
            ),
            NativeFunction::CollectionGet => self.dispatch_collections_native(
                dom,
                NativeFunction::CollectionGet,
                receiver,
                arguments,
            ),
            NativeFunction::CollectionHas => self.dispatch_collections_native(
                dom,
                NativeFunction::CollectionHas,
                receiver,
                arguments,
            ),
            NativeFunction::CollectionIteratorNext => self.dispatch_collections_native(
                dom,
                NativeFunction::CollectionIteratorNext,
                receiver,
                arguments,
            ),
            NativeFunction::CollectionKeys => self.dispatch_collections_native(
                dom,
                NativeFunction::CollectionKeys,
                receiver,
                arguments,
            ),
            NativeFunction::CollectionSet => self.dispatch_collections_native(
                dom,
                NativeFunction::CollectionSet,
                receiver,
                arguments,
            ),
            NativeFunction::CollectionValues => self.dispatch_collections_native(
                dom,
                NativeFunction::CollectionValues,
                receiver,
                arguments,
            ),
            NativeFunction::CompareDocumentPosition => self.dispatch_dom_native(
                dom,
                NativeFunction::CompareDocumentPosition,
                receiver,
                arguments,
            ),
            NativeFunction::Contains => {
                self.dispatch_dom_native(dom, NativeFunction::Contains, receiver, arguments)
            }
            NativeFunction::CreateDocumentFragment => self.dispatch_dom_native(
                dom,
                NativeFunction::CreateDocumentFragment,
                receiver,
                arguments,
            ),
            NativeFunction::CreateElement => {
                self.dispatch_dom_native(dom, NativeFunction::CreateElement, receiver, arguments)
            }
            NativeFunction::CreateTextNode => {
                self.dispatch_dom_native(dom, NativeFunction::CreateTextNode, receiver, arguments)
            }
            NativeFunction::DateGetDate => {
                self.dispatch_date_native(dom, NativeFunction::DateGetDate, receiver, arguments)
            }
            NativeFunction::DateGetDay => {
                self.dispatch_date_native(dom, NativeFunction::DateGetDay, receiver, arguments)
            }
            NativeFunction::DateGetFullYear => {
                self.dispatch_date_native(dom, NativeFunction::DateGetFullYear, receiver, arguments)
            }
            NativeFunction::DateGetHours => {
                self.dispatch_date_native(dom, NativeFunction::DateGetHours, receiver, arguments)
            }
            NativeFunction::DateGetMilliseconds => self.dispatch_date_native(
                dom,
                NativeFunction::DateGetMilliseconds,
                receiver,
                arguments,
            ),
            NativeFunction::DateGetMinutes => {
                self.dispatch_date_native(dom, NativeFunction::DateGetMinutes, receiver, arguments)
            }
            NativeFunction::DateGetMonth => {
                self.dispatch_date_native(dom, NativeFunction::DateGetMonth, receiver, arguments)
            }
            NativeFunction::DateGetSeconds => {
                self.dispatch_date_native(dom, NativeFunction::DateGetSeconds, receiver, arguments)
            }
            NativeFunction::DateGetTimezoneOffset => self.dispatch_date_native(
                dom,
                NativeFunction::DateGetTimezoneOffset,
                receiver,
                arguments,
            ),
            NativeFunction::DateGetUTCDate => {
                self.dispatch_date_native(dom, NativeFunction::DateGetUTCDate, receiver, arguments)
            }
            NativeFunction::DateGetUTCDay => {
                self.dispatch_date_native(dom, NativeFunction::DateGetUTCDay, receiver, arguments)
            }
            NativeFunction::DateGetUTCFullYear => self.dispatch_date_native(
                dom,
                NativeFunction::DateGetUTCFullYear,
                receiver,
                arguments,
            ),
            NativeFunction::DateGetUTCHours => {
                self.dispatch_date_native(dom, NativeFunction::DateGetUTCHours, receiver, arguments)
            }
            NativeFunction::DateGetUTCMilliseconds => self.dispatch_date_native(
                dom,
                NativeFunction::DateGetUTCMilliseconds,
                receiver,
                arguments,
            ),
            NativeFunction::DateGetUTCMinutes => self.dispatch_date_native(
                dom,
                NativeFunction::DateGetUTCMinutes,
                receiver,
                arguments,
            ),
            NativeFunction::DateGetUTCMonth => {
                self.dispatch_date_native(dom, NativeFunction::DateGetUTCMonth, receiver, arguments)
            }
            NativeFunction::DateGetUTCSeconds => self.dispatch_date_native(
                dom,
                NativeFunction::DateGetUTCSeconds,
                receiver,
                arguments,
            ),
            NativeFunction::DateGetValue => {
                self.dispatch_date_native(dom, NativeFunction::DateGetValue, receiver, arguments)
            }
            NativeFunction::DateNow => {
                self.dispatch_date_native(dom, NativeFunction::DateNow, receiver, arguments)
            }
            NativeFunction::DateParse => {
                self.dispatch_date_native(dom, NativeFunction::DateParse, receiver, arguments)
            }
            NativeFunction::DateSetTime => {
                self.dispatch_date_native(dom, NativeFunction::DateSetTime, receiver, arguments)
            }
            NativeFunction::DateToDateString => self.dispatch_date_native(
                dom,
                NativeFunction::DateToDateString,
                receiver,
                arguments,
            ),
            NativeFunction::DateToGMTString => {
                self.dispatch_date_native(dom, NativeFunction::DateToGMTString, receiver, arguments)
            }
            NativeFunction::DateToISOString => {
                self.dispatch_date_native(dom, NativeFunction::DateToISOString, receiver, arguments)
            }
            NativeFunction::DateToJSON => {
                self.dispatch_date_native(dom, NativeFunction::DateToJSON, receiver, arguments)
            }
            NativeFunction::DateToString => {
                self.dispatch_date_native(dom, NativeFunction::DateToString, receiver, arguments)
            }
            NativeFunction::DateUTC => {
                self.dispatch_date_native(dom, NativeFunction::DateUTC, receiver, arguments)
            }
            NativeFunction::DateValueOf => {
                self.dispatch_date_native(dom, NativeFunction::DateValueOf, receiver, arguments)
            }
            NativeFunction::ErrorPrototypeToString => self.dispatch_object_native(
                dom,
                NativeFunction::ErrorPrototypeToString,
                receiver,
                arguments,
            ),
            NativeFunction::EventPreventDefault => self.dispatch_events_native(
                dom,
                NativeFunction::EventPreventDefault,
                receiver,
                arguments,
            ),
            NativeFunction::GetAttribute => {
                self.dispatch_dom_native(dom, NativeFunction::GetAttribute, receiver, arguments)
            }
            NativeFunction::GetComputedStyle => {
                self.dispatch_dom_native(dom, NativeFunction::GetComputedStyle, receiver, arguments)
            }
            NativeFunction::GetElementById => {
                self.dispatch_dom_native(dom, NativeFunction::GetElementById, receiver, arguments)
            }
            NativeFunction::GetElementsByClassName => self.dispatch_dom_native(
                dom,
                NativeFunction::GetElementsByClassName,
                receiver,
                arguments,
            ),
            NativeFunction::GetElementsByTagName => self.dispatch_dom_native(
                dom,
                NativeFunction::GetElementsByTagName,
                receiver,
                arguments,
            ),
            NativeFunction::HasAttribute => {
                self.dispatch_dom_native(dom, NativeFunction::HasAttribute, receiver, arguments)
            }
            NativeFunction::InsertBefore => {
                self.dispatch_dom_native(dom, NativeFunction::InsertBefore, receiver, arguments)
            }
            NativeFunction::IntersectionDisconnect => self.dispatch_observers_native(
                dom,
                NativeFunction::IntersectionDisconnect,
                receiver,
                arguments,
            ),
            NativeFunction::IntersectionObserve => self.dispatch_observers_native(
                dom,
                NativeFunction::IntersectionObserve,
                receiver,
                arguments,
            ),
            NativeFunction::IntersectionTakeRecords => self.dispatch_observers_native(
                dom,
                NativeFunction::IntersectionTakeRecords,
                receiver,
                arguments,
            ),
            NativeFunction::IntersectionUnobserve => self.dispatch_observers_native(
                dom,
                NativeFunction::IntersectionUnobserve,
                receiver,
                arguments,
            ),
            NativeFunction::JsonParse => {
                self.dispatch_json_native(dom, NativeFunction::JsonParse, receiver, arguments)
            }
            NativeFunction::JsonStringify => {
                self.dispatch_json_native(dom, NativeFunction::JsonStringify, receiver, arguments)
            }
            NativeFunction::Matches => {
                self.dispatch_dom_native(dom, NativeFunction::Matches, receiver, arguments)
            }
            NativeFunction::MathAbs => {
                self.dispatch_math_native(dom, NativeFunction::MathAbs, receiver, arguments)
            }
            NativeFunction::MathCeil => {
                self.dispatch_math_native(dom, NativeFunction::MathCeil, receiver, arguments)
            }
            NativeFunction::MathFloor => {
                self.dispatch_math_native(dom, NativeFunction::MathFloor, receiver, arguments)
            }
            NativeFunction::MathMax => {
                self.dispatch_math_native(dom, NativeFunction::MathMax, receiver, arguments)
            }
            NativeFunction::MathMin => {
                self.dispatch_math_native(dom, NativeFunction::MathMin, receiver, arguments)
            }
            NativeFunction::MathPow => {
                self.dispatch_math_native(dom, NativeFunction::MathPow, receiver, arguments)
            }
            NativeFunction::MathRandom => {
                self.dispatch_math_native(dom, NativeFunction::MathRandom, receiver, arguments)
            }
            NativeFunction::MathRound => {
                self.dispatch_math_native(dom, NativeFunction::MathRound, receiver, arguments)
            }
            NativeFunction::MathSqrt => {
                self.dispatch_math_native(dom, NativeFunction::MathSqrt, receiver, arguments)
            }
            NativeFunction::MutationDisconnect => self.dispatch_observers_native(
                dom,
                NativeFunction::MutationDisconnect,
                receiver,
                arguments,
            ),
            NativeFunction::MutationObserve => self.dispatch_observers_native(
                dom,
                NativeFunction::MutationObserve,
                receiver,
                arguments,
            ),
            NativeFunction::MutationTakeRecords => self.dispatch_observers_native(
                dom,
                NativeFunction::MutationTakeRecords,
                receiver,
                arguments,
            ),
            NativeFunction::NamedMapGetNamedItem => self.dispatch_dom_native(
                dom,
                NativeFunction::NamedMapGetNamedItem,
                receiver,
                arguments,
            ),
            NativeFunction::NamedMapItem => {
                self.dispatch_dom_native(dom, NativeFunction::NamedMapItem, receiver, arguments)
            }
            NativeFunction::NumToFixed => {
                self.dispatch_object_native(dom, NativeFunction::NumToFixed, receiver, arguments)
            }
            NativeFunction::NumToPrecision => self.dispatch_object_native(
                dom,
                NativeFunction::NumToPrecision,
                receiver,
                arguments,
            ),
            NativeFunction::NumToString => {
                self.dispatch_object_native(dom, NativeFunction::NumToString, receiver, arguments)
            }
            NativeFunction::NumValueOf => {
                self.dispatch_object_native(dom, NativeFunction::NumValueOf, receiver, arguments)
            }
            NativeFunction::ObjectAssign => {
                self.dispatch_object_native(dom, NativeFunction::ObjectAssign, receiver, arguments)
            }
            NativeFunction::ObjectCreate => {
                self.dispatch_object_native(dom, NativeFunction::ObjectCreate, receiver, arguments)
            }
            NativeFunction::ObjectDefineProperties => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectDefineProperties,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectDefineProperty => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectDefineProperty,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectEntries => {
                self.dispatch_object_native(dom, NativeFunction::ObjectEntries, receiver, arguments)
            }
            NativeFunction::ObjectGetOwnPropertyDescriptor => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectGetOwnPropertyDescriptor,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectGetOwnPropertyDescriptors => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectGetOwnPropertyDescriptors,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectGetOwnPropertyNames => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectGetOwnPropertyNames,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectGetOwnPropertySymbols => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectGetOwnPropertySymbols,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectGetPrototypeOf => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectGetPrototypeOf,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectHasOwn => {
                self.dispatch_object_native(dom, NativeFunction::ObjectHasOwn, receiver, arguments)
            }
            NativeFunction::ObjectKeys => {
                self.dispatch_object_native(dom, NativeFunction::ObjectKeys, receiver, arguments)
            }
            NativeFunction::ObjectPrototypeHasOwnProperty => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectPrototypeHasOwnProperty,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectPrototypeIsPrototypeOf => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectPrototypeIsPrototypeOf,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectPrototypePropertyIsEnumerable => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectPrototypePropertyIsEnumerable,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectPrototypeToString => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectPrototypeToString,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectPrototypeValueOf => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectPrototypeValueOf,
                receiver,
                arguments,
            ),
            NativeFunction::ObjectValues => {
                self.dispatch_object_native(dom, NativeFunction::ObjectValues, receiver, arguments)
            }
            NativeFunction::PromiseCatch => {
                self.dispatch_promise_native(dom, NativeFunction::PromiseCatch, receiver, arguments)
            }
            NativeFunction::PromiseReject => self.dispatch_promise_native(
                dom,
                NativeFunction::PromiseReject,
                receiver,
                arguments,
            ),
            NativeFunction::PromiseResolve => self.dispatch_promise_native(
                dom,
                NativeFunction::PromiseResolve,
                receiver,
                arguments,
            ),
            NativeFunction::PromiseThen => {
                self.dispatch_promise_native(dom, NativeFunction::PromiseThen, receiver, arguments)
            }
            NativeFunction::QuerySelector => {
                self.dispatch_dom_native(dom, NativeFunction::QuerySelector, receiver, arguments)
            }
            NativeFunction::QuerySelectorAll => {
                self.dispatch_dom_native(dom, NativeFunction::QuerySelectorAll, receiver, arguments)
            }
            NativeFunction::RegExpExec => {
                self.dispatch_regexp_native(dom, NativeFunction::RegExpExec, receiver, arguments)
            }
            NativeFunction::RegExpTest => {
                self.dispatch_regexp_native(dom, NativeFunction::RegExpTest, receiver, arguments)
            }
            NativeFunction::RegExpToString => self.dispatch_regexp_native(
                dom,
                NativeFunction::RegExpToString,
                receiver,
                arguments,
            ),
            NativeFunction::RemoveAttribute => {
                self.dispatch_dom_native(dom, NativeFunction::RemoveAttribute, receiver, arguments)
            }
            NativeFunction::RemoveChild => {
                self.dispatch_dom_native(dom, NativeFunction::RemoveChild, receiver, arguments)
            }
            NativeFunction::RemoveNode => {
                self.dispatch_dom_native(dom, NativeFunction::RemoveNode, receiver, arguments)
            }
            NativeFunction::SetAttribute => self.dispatch_collections_native(
                dom,
                NativeFunction::SetAttribute,
                receiver,
                arguments,
            ),
            NativeFunction::SetInterval => self.dispatch_collections_native(
                dom,
                NativeFunction::SetInterval,
                receiver,
                arguments,
            ),
            NativeFunction::SetTimeout => self.dispatch_collections_native(
                dom,
                NativeFunction::SetTimeout,
                receiver,
                arguments,
            ),
            NativeFunction::StringFromCharCode => self.dispatch_string_native(
                dom,
                NativeFunction::StringFromCharCode,
                receiver,
                arguments,
            ),
            NativeFunction::StringFromCodePoint => self.dispatch_string_native(
                dom,
                NativeFunction::StringFromCodePoint,
                receiver,
                arguments,
            ),
            NativeFunction::StringRaw => {
                self.dispatch_string_native(dom, NativeFunction::StringRaw, receiver, arguments)
            }
            NativeFunction::StringSubstr => {
                self.dispatch_string_native(dom, NativeFunction::StringSubstr, receiver, arguments)
            }
            NativeFunction::StyleGetProperty => self.dispatch_style_native(
                dom,
                NativeFunction::StyleGetProperty,
                receiver,
                arguments,
            ),
            NativeFunction::StyleItem => {
                self.dispatch_style_native(dom, NativeFunction::StyleItem, receiver, arguments)
            }
            NativeFunction::StyleRemoveProperty => self.dispatch_style_native(
                dom,
                NativeFunction::StyleRemoveProperty,
                receiver,
                arguments,
            ),
            NativeFunction::StyleSetProperty => self.dispatch_style_native(
                dom,
                NativeFunction::StyleSetProperty,
                receiver,
                arguments,
            ),
            NativeFunction::SymbolToString => self.dispatch_object_native(
                dom,
                NativeFunction::SymbolToString,
                receiver,
                arguments,
            ),
            NativeFunction::SymbolValueOf => {
                self.dispatch_object_native(dom, NativeFunction::SymbolValueOf, receiver, arguments)
            }
            NativeFunction::TypedArrayFill => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArrayFill,
                receiver,
                arguments,
            ),
            NativeFunction::TypedArrayFilter => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArrayFilter,
                receiver,
                arguments,
            ),
            NativeFunction::TypedArrayForEach => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArrayForEach,
                receiver,
                arguments,
            ),
            NativeFunction::TypedArrayFrom => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArrayFrom,
                receiver,
                arguments,
            ),
            NativeFunction::TypedArrayIncludes => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArrayIncludes,
                receiver,
                arguments,
            ),
            NativeFunction::TypedArrayIndexOf => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArrayIndexOf,
                receiver,
                arguments,
            ),
            NativeFunction::TypedArrayJoin => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArrayJoin,
                receiver,
                arguments,
            ),
            NativeFunction::TypedArrayMap => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArrayMap,
                receiver,
                arguments,
            ),
            NativeFunction::TypedArraySet => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArraySet,
                receiver,
                arguments,
            ),
            NativeFunction::TypedArraySlice => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArraySlice,
                receiver,
                arguments,
            ),
            NativeFunction::TypedArraySubarray => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArraySubarray,
                receiver,
                arguments,
            ),
            NativeFunction::UrlSearchParamsAppend => self.dispatch_url_native(
                dom,
                NativeFunction::UrlSearchParamsAppend,
                receiver,
                arguments,
            ),
            NativeFunction::UrlSearchParamsForEach => self.dispatch_url_native(
                dom,
                NativeFunction::UrlSearchParamsForEach,
                receiver,
                arguments,
            ),
            NativeFunction::UrlSearchParamsGet => self.dispatch_url_native(
                dom,
                NativeFunction::UrlSearchParamsGet,
                receiver,
                arguments,
            ),
            NativeFunction::UrlSearchParamsHas => self.dispatch_url_native(
                dom,
                NativeFunction::UrlSearchParamsHas,
                receiver,
                arguments,
            ),
            NativeFunction::UrlSearchParamsSet => self.dispatch_url_native(
                dom,
                NativeFunction::UrlSearchParamsSet,
                receiver,
                arguments,
            ),
            NativeFunction::UrlSearchParamsToString => self.dispatch_url_native(
                dom,
                NativeFunction::UrlSearchParamsToString,
                receiver,
                arguments,
            ),
            NativeFunction::UrlToString => {
                self.dispatch_url_native(dom, NativeFunction::UrlToString, receiver, arguments)
            }
            NativeFunction::WindowAddEventListener => self.dispatch_events_native(
                dom,
                NativeFunction::WindowAddEventListener,
                receiver,
                arguments,
            ),
            NativeFunction::WindowRemoveEventListener => self.dispatch_events_native(
                dom,
                NativeFunction::WindowRemoveEventListener,
                receiver,
                arguments,
            ),
        }
    }
}

impl JsRuntime {
    /// Format `console.*` arguments the way engines join them: one space
    /// between arguments, objects through their string coercion.
    pub(in crate::runtime) fn console_write(
        &mut self,
        function: NativeFunction,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let level = match function {
            NativeFunction::ConsoleDebug => ConsoleLevel::Debug,
            NativeFunction::ConsoleError => ConsoleLevel::Error,
            NativeFunction::ConsoleInfo => ConsoleLevel::Info,
            NativeFunction::ConsoleLog => ConsoleLevel::Log,
            NativeFunction::ConsoleWarn => ConsoleLevel::Warn,
            _ => return Err(JsError::type_error("incompatible console method receiver")),
        };
        let text = arguments
            .iter()
            .map(JsValue::to_js_string)
            .collect::<Vec<_>>()
            .join(" ");
        if self.console_messages.len() >= MAX_BUFFERED_CONSOLE_MESSAGES {
            self.console_messages.remove(0);
        }
        self.console_messages.push(ConsoleMessage { level, text });
        Ok(JsValue::Undefined)
    }
}
