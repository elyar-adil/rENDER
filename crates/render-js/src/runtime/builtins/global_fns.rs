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

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_exception::DomExceptionName;
use crate::runtime::convert::required_argument;
use crate::runtime::convert::to_number;
use crate::runtime::types::ConsoleLevel;
use crate::runtime::types::ConsoleMessage;
use crate::runtime::types::JsMicrotask;
use crate::runtime::types::MAX_BUFFERED_CONSOLE_MESSAGES;
use crate::runtime::types::TimerKind;
use crate::utf16;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;
use std::fmt::Write as _;

impl JsRuntime {
    fn base64_encode(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let first = chunk[0];
            let second = chunk.get(1).copied().unwrap_or(0);
            let third = chunk.get(2).copied().unwrap_or(0);
            output.push(TABLE[(first >> 2) as usize] as char);
            output.push(TABLE[((first & 0x03) << 4 | second >> 4) as usize] as char);
            if chunk.len() > 1 {
                output.push(TABLE[((second & 0x0f) << 2 | third >> 6) as usize] as char);
            } else {
                output.push('=');
            }
            if chunk.len() > 2 {
                output.push(TABLE[(third & 0x3f) as usize] as char);
            } else {
                output.push('=');
            }
        }
        output
    }

    /// Infra §"forgiving-base64 decode".
    ///
    /// Every rejection in the algorithm is the same exception: "throw an
    /// `InvalidCharacterError` `DOMException`". Making that a `DOMException`
    /// rather than the engine's generic `Error` is what lets
    /// `catch (e) { if (e.name === "InvalidCharacterError") ... }` - the form
    /// every base64 wrapper in the wild uses - take the right branch, and what
    /// makes `e instanceof DOMException` true instead of false.
    fn base64_decode(&mut self, text: &str) -> Result<String, JsError> {
        let invalid = |runtime: &mut Self| {
            runtime.dom_exception(
                DomExceptionName::InvalidCharacter,
                "The string to be decoded contains invalid characters",
            )
        };
        let compact = text
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect::<String>();
        if compact.is_empty() {
            return Ok(String::new());
        }
        if compact.len() % 4 != 0 {
            return Err(invalid(self));
        }
        let value = |character: u8| -> Option<u8> {
            match character {
                b'A'..=b'Z' => Some(character - b'A'),
                b'a'..=b'z' => Some(character - b'a' + 26),
                b'0'..=b'9' => Some(character - b'0' + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        };
        let bytes = compact.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len() / 4 * 3);
        for chunk in bytes.chunks_exact(4) {
            let Some(first) = value(chunk[0]) else {
                return Err(invalid(self));
            };
            let Some(second) = value(chunk[1]) else {
                return Err(invalid(self));
            };
            decoded.push((first << 2) | (second >> 4));
            if chunk[2] != b'=' {
                let Some(third) = value(chunk[2]) else {
                    return Err(invalid(self));
                };
                decoded.push((second << 4) | (third >> 2));
                if chunk[3] != b'=' {
                    let Some(fourth) = value(chunk[3]) else {
                        return Err(invalid(self));
                    };
                    decoded.push((third << 6) | fourth);
                }
            }
        }
        Ok(decoded.into_iter().map(char::from).collect())
    }

    pub(in crate::runtime) fn dispatch_residual_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::DomExceptionNameGetter
            | NativeFunction::DomExceptionMessageGetter
            | NativeFunction::DomExceptionCodeGetter => {
                self.dom_exception_accessor(receiver, function)
            }
            // `matchMedia` and the `MediaQueryList` listener methods are
            // observer machinery, so they are dispatched from `observers.rs`;
            // listing them here is what makes a missing arm a compile error
            // rather than a `TypeError` at runtime.
            NativeFunction::WindowMatchMedia
            | NativeFunction::MediaQueryListMediaGetter
            | NativeFunction::MediaQueryListMatchesGetter
            | NativeFunction::MediaQueryListAddEventListener
            | NativeFunction::MediaQueryListRemoveEventListener
            | NativeFunction::MediaQueryListAddListener
            | NativeFunction::MediaQueryListRemoveListener => {
                self.dispatch_observers_native(dom, function, receiver, arguments)
            }
            NativeFunction::CreateComment
            | NativeFunction::ReflectGet
            | NativeFunction::ReflectSet
            | NativeFunction::ReflectHas
            | NativeFunction::ReflectDeleteProperty
            | NativeFunction::ReflectOwnKeys
            | NativeFunction::ReflectGetOwnPropertyDescriptor
            | NativeFunction::ReflectDefineProperty
            | NativeFunction::ReflectConstruct
            | NativeFunction::ReflectApply
            | NativeFunction::ReflectGetPrototypeOf
            | NativeFunction::ReflectSetPrototypeOf
            | NativeFunction::ReflectIsExtensible
            | NativeFunction::ReflectPreventExtensions => {
                self.dispatch_proxy_native(dom, function, receiver, arguments)
            }
            NativeFunction::StorageGetItem
            | NativeFunction::StorageSetItem
            | NativeFunction::StorageRemoveItem
            | NativeFunction::StorageClear
            | NativeFunction::StorageKey => {
                self.dispatch_storage_native(dom, function, receiver, arguments)
            }
            // Fetch-domain functions are intercepted at the dispatch head in
            // `fetch.rs`; these arms exist so a new variant stays a compile
            // error here instead of a silent runtime gap.
            NativeFunction::GlobalFetch
            | NativeFunction::ResponseText
            | NativeFunction::ResponseJson
            | NativeFunction::ResponseHeadersGet
            | NativeFunction::BlobText
            | NativeFunction::BlobArrayBuffer
            | NativeFunction::BlobSlice
            | NativeFunction::XhrOpen
            | NativeFunction::XhrSetRequestHeader
            | NativeFunction::XhrSend
            | NativeFunction::XhrGetResponseHeader
            | NativeFunction::XhrGetAllResponseHeaders
            | NativeFunction::XhrAddEventListener
            | NativeFunction::XhrRemoveEventListener
            | NativeFunction::AbortControllerAbort
            | NativeFunction::FormDataAppend
            | NativeFunction::FormDataGet
            | NativeFunction::FormDataSet
            | NativeFunction::FormDataHas
            | NativeFunction::FormDataDelete
            | NativeFunction::FormDataEntries => {
                self.dispatch_fetch_native(dom, function, receiver, arguments)
            }
            // The Encoding Standard and `DataView` share one dispatcher so the
            // chain has a single entry point for the buffer-oriented globals.
            NativeFunction::TextEncoderEncode
            | NativeFunction::TextEncoderEncodeInto
            | NativeFunction::TextDecoderDecode
            | NativeFunction::DataViewGetInt8
            | NativeFunction::DataViewGetUint8
            | NativeFunction::DataViewGetInt16
            | NativeFunction::DataViewGetUint16
            | NativeFunction::DataViewGetInt32
            | NativeFunction::DataViewGetUint32
            | NativeFunction::DataViewGetFloat32
            | NativeFunction::DataViewGetFloat64
            | NativeFunction::DataViewSetInt8
            | NativeFunction::DataViewSetUint8
            | NativeFunction::DataViewSetInt16
            | NativeFunction::DataViewSetUint16
            | NativeFunction::DataViewSetInt32
            | NativeFunction::DataViewSetUint32
            | NativeFunction::DataViewSetFloat32
            | NativeFunction::DataViewSetFloat64
            | NativeFunction::ArrayBufferSlice => {
                self.dispatch_encoding_native(dom, function, receiver, arguments)
            }
            NativeFunction::GlobalStructuredClone => {
                self.dispatch_structured_clone_native(dom, function, receiver, arguments)
            }
            NativeFunction::UrlCreateObjectUrl | NativeFunction::UrlRevokeObjectUrl => {
                self.dispatch_url_native(dom, function, receiver, arguments)
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
            NativeFunction::ObjectProtoGetter | NativeFunction::ObjectProtoSetter => {
                self.dispatch_object_native(dom, function, receiver, arguments)
            }
            NativeFunction::ObjectPreventExtensions => {
                let Some(object) = self.integrity_target(arguments, "preventExtensions")? else {
                    return Err(JsError::type_error(
                        "Object.preventExtensions called on null or undefined",
                    ));
                };
                self.realm.prevent_extensions(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectSeal => {
                let Some(object) = self.integrity_target(arguments, "seal")? else {
                    return Err(JsError::type_error(
                        "Object.seal called on null or undefined",
                    ));
                };
                self.realm.seal_object(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectFreeze => {
                let Some(object) = self.integrity_target(arguments, "freeze")? else {
                    return Err(JsError::type_error(
                        "Object.freeze called on null or undefined",
                    ));
                };
                self.realm.freeze_object(object);
                Ok(JsValue::Object(object))
            }
            NativeFunction::ObjectIsExtensible => {
                let Some(object) = self.integrity_target(arguments, "isExtensible")? else {
                    return Err(JsError::type_error(
                        "Object.isExtensible called on null or undefined",
                    ));
                };
                Ok(JsValue::Boolean(self.realm.is_extensible(object)))
            }
            NativeFunction::ObjectIsSealed => {
                // §20.1.2.13: a non-object target is always sealed.
                let Some(object) = self.integrity_target(arguments, "isSealed")? else {
                    return Ok(JsValue::Boolean(true));
                };
                Ok(JsValue::Boolean(self.realm.is_sealed(object)))
            }
            NativeFunction::ObjectIsFrozen => {
                // §20.1.2.14: a non-object target is always frozen.
                let Some(object) = self.integrity_target(arguments, "isFrozen")? else {
                    return Ok(JsValue::Boolean(true));
                };
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
            NativeFunction::ArrayAt
            | NativeFunction::ArrayFlat
            | NativeFunction::ArrayReduceRight
            | NativeFunction::ArrayFindLast
            | NativeFunction::ArrayFindLastIndex
            | NativeFunction::StrCodePointAt
            | NativeFunction::StrAt
            | NativeFunction::StrPadStart
            | NativeFunction::StrPadEnd
            | NativeFunction::StrTrimStart
            | NativeFunction::StrTrimEnd
            | NativeFunction::StrRepeat
            | NativeFunction::StrLocaleCompare
            | NativeFunction::StrReplaceAll => {
                self.dispatch_string_native(dom, function, receiver, arguments)
            }
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
                // This is an engine extension rather than a specified method,
                // so it *walks* the string the way the iterator does and
                // yields code points - but the index it reports has to be a
                // position in the string, so it is the code-unit offset of
                // each code point rather than a running counter.
                for (offset, character) in text.chars().scan(0usize, |offset, character| {
                    let start = *offset;
                    *offset += character.len_utf16();
                    Some((start, character))
                }) {
                    #[allow(clippy::cast_precision_loss)]
                    let index = JsValue::Number(offset as f64);
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
            NativeFunction::StrPush => Ok(JsValue::Number(utf16::utf16_length(
                &self.require_string_receiver(receiver)?,
            ) as f64)),
            NativeFunction::StrIterator => self.string_iterator(receiver),
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
            NativeFunction::CssSupports => css_supports(arguments),
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
            NativeFunction::GlobalAtob => {
                let text = required_argument(arguments, 0, "atob")?.to_js_string();
                self.base64_decode(&text).map(JsValue::String)
            }
            NativeFunction::GlobalBtoa => {
                let text = required_argument(arguments, 0, "btoa")?.to_js_string();
                // Infra §"base64 encode": "If the code point value of any
                // character in data is greater than 255, then throw an
                // `InvalidCharacterError` `DOMException`."
                if text.chars().any(|character| character as u32 > 0xff) {
                    return Err(self.dom_exception(
                        DomExceptionName::InvalidCharacter,
                        "The string to be encoded contains characters outside of the Latin1 range",
                    ));
                }
                Ok(JsValue::String(Self::base64_encode(
                    &text.bytes().collect::<Vec<_>>(),
                )))
            }
            NativeFunction::GlobalParseInt => {
                // ECMA-262 7.1.1.1. The radix argument used to be ignored, so
                // every `parseInt(hex, 16)` in a bundle came back `NaN`.
                let text = required_argument(arguments, 0, "parseInt")?.to_js_string();
                Ok(JsValue::Number(crate::runtime::convert::parse_int(
                    &text,
                    arguments.get(1),
                )?))
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
            NativeFunction::PerformanceGetEntries | NativeFunction::PerformanceGetEntriesByType => {
                Ok(JsValue::Object(self.create_array_from_values(&[])?))
            }
            NativeFunction::FunctionPrototype
            | NativeFunction::FunctionToString
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
            NativeFunction::ArrayValues
            | NativeFunction::ArrayKeys
            | NativeFunction::ArrayEntries => {
                self.dispatch_array_native(dom, function, receiver, arguments)
            }
            NativeFunction::IteratorConstructor
            | NativeFunction::IteratorFrom
            | NativeFunction::IteratorPrototypeIterator
            | NativeFunction::IteratorHelperNext
            | NativeFunction::IteratorHelperReturn
            | NativeFunction::IteratorMap
            | NativeFunction::IteratorFilter
            | NativeFunction::IteratorTake
            | NativeFunction::IteratorDrop
            | NativeFunction::IteratorFlatMap
            | NativeFunction::IteratorReduce
            | NativeFunction::IteratorToArray
            | NativeFunction::IteratorForEach
            | NativeFunction::IteratorSome
            | NativeFunction::IteratorEvery
            | NativeFunction::IteratorFind
            | NativeFunction::IteratorConcat
            | NativeFunction::IteratorChunks
            | NativeFunction::IteratorWindows => self
                .dispatch_iterator_native(dom, function, receiver, arguments)
                .unwrap_or_else(|| Err(JsError::type_error("iterator helper dispatch failed"))),
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
            NativeFunction::CreateEvent => {
                self.dispatch_dom_native(dom, NativeFunction::CreateEvent, receiver, arguments)
            }
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
            NativeFunction::MathOp(_) | NativeFunction::NumberOp(_) => {
                self.dispatch_math_native(dom, function, receiver, arguments)
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
            NativeFunction::ObjectSetPrototypeOf => self.dispatch_object_native(
                dom,
                NativeFunction::ObjectSetPrototypeOf,
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
            NativeFunction::TypedArrayValues => self.dispatch_typed_array_native(
                dom,
                NativeFunction::TypedArrayValues,
                receiver,
                arguments,
            ),
            NativeFunction::PromiseAll
            | NativeFunction::PromiseAllSettled
            | NativeFunction::PromiseAny
            | NativeFunction::PromiseRace
            | NativeFunction::PromiseCombinatorFulfilled
            | NativeFunction::PromiseCombinatorRejected => {
                self.dispatch_promise_native(dom, function, receiver, arguments)
            }
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

/// CSSOM §"supports": the two overloads of the `CSS` namespace's static
/// `supports`, dispatched by argument count.
///
/// CSSOM declares
///
/// ```idl
/// partial interface CSS {
///   static boolean supports(DOMString property, DOMString value);
///   static boolean supports(DOMString conditionText);
/// };
/// ```
///
/// and `WebIDL` §3.6 overload resolution picks the first alternative whose
/// required-argument count the call satisfies, so two arguments select the
/// declaration form and one selects the condition form. The two forms answer
/// different questions and a hardcoded `true` answers both wrongly, which is the
/// failure `@supports` exists to prevent: an author writes
/// `@supports (backdrop-filter: blur(2px))` precisely to reach a fallback this
/// engine does not implement, and answering `true` tells the page the fallback
/// is unnecessary.
///
/// The answers come from `render-css`, which is the crate that already decides
/// whether a declaration survives a style rule, so one parser and one
/// evaluator answer both the stylesheet question and the script question.
fn css_supports(arguments: &[JsValue]) -> Result<JsValue, JsError> {
    let first = required_argument(arguments, 0, "CSS.supports")?;
    if let Some(second) = arguments.get(1) {
        return Ok(JsValue::Boolean(
            render_css::supports::supports_declaration(
                &first.to_js_string(),
                &second.to_js_string(),
            ),
        ));
    }
    Ok(JsValue::Boolean(
        render_css::supports::supports_condition_text(&first.to_js_string()),
    ))
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

#[cfg(test)]
mod tests {
    use super::css_supports;
    use crate::JsValue;

    fn supports(arguments: &[&str]) -> String {
        let values: Vec<JsValue> = arguments
            .iter()
            .map(|argument| JsValue::String((*argument).to_owned()))
            .collect();
        css_supports(&values)
            .expect("CSS.supports should answer")
            .to_js_string()
    }

    /// `CSS.supports` was a hardcoded `true`, which is the failure
    /// `@supports` exists to prevent. The decisive assertion is the *false*
    /// side: a query for a feature this engine does not implement has to answer
    /// `false`, because that is the answer that makes an author's fallback apply.
    /// A test that only asserted the `true` side would pass against the stub.
    #[test]
    fn css_supports_answers_false_for_a_feature_the_engine_lacks() {
        assert_eq!(supports(&["backdrop-filter", "blur(2px)"]), "false");
        assert_eq!(supports(&["(backdrop-filter: blur(2px))"]), "false");
        assert_eq!(supports(&["(text-overflow: ellipsis)"]), "false");
        assert_eq!(supports(&["(-moz-box-shadow: 0 0 2px black)"]), "false");
        // A feature no specification defines.
        assert_eq!(supports(&["(nonesuch-property: 1)"]), "false");
        // A named condition this engine does not define, and a general-enclosed
        // one, are both `false` rather than an error: CSS Conditional Rules 3 §6
        // fixes that production at false precisely so new syntax does not
        // invalidate too much of a condition.
        assert_eq!(supports(&["(some-named-condition)"]), "false");
        assert_eq!(supports(&["(some future feature)"]), "false");
    }

    /// The `true` side, and the part of it that is a capability claim: a
    /// declaration the engine's own cascade accepts is a declaration it supports,
    /// because the answer comes from `render-css`'s declaration oracle rather
    /// than from a list.
    #[test]
    fn css_supports_answers_from_the_cascade_for_the_true_side_too() {
        assert_eq!(supports(&["display", "grid"]), "true");
        assert_eq!(supports(&["display", "flex"]), "true");
        assert_eq!(supports(&["(display: grid)"]), "true");
        assert_eq!(supports(&["(color: rgb(1, 2, 3))"]), "true");
        // `render-layout`'s text carries no family, so no engine grammar for
        // `font-family` exists and §6.1 says that is unsupported.
        assert_eq!(supports(&["(font-family: Arial)"]), "false");
        // A shorthand is the conjunction over its longhands, so `font` is
        // unsupported for exactly that reason.
        assert_eq!(supports(&["(font: 12px/1.5 Arial)"]), "false");
    }

    /// CSS Conditional Rules 3 §7.5's two overloads and the wrapped retry, which
    /// is a second parse and not the first answer reused.
    #[test]
    fn css_supports_keeps_the_sevenths_five_wrapped_retry_and_overload_split() {
        // The retry: bare `display: grid` is not a `<supports-condition>`, and
        // wrapping it in parentheses is.
        assert_eq!(supports(&["display: grid"]), "true");
        assert_eq!(supports(&["(display: grid)"]), "true");
        // `not (display: grid)` is false, and wrapping it does not make it true.
        assert_eq!(supports(&["not (display: grid)"]), "false");
        assert_eq!(supports(&["not (backdrop-filter: blur(2px))"]), "true");
        // The two-argument form is the declaration one even when the text looks
        // like a condition, because WebIDL §3.6 picks the first overload the
        // argument count satisfies.
        assert_eq!(supports(&["display: grid", ""]), "false");
        assert_eq!(supports(&["display", "grid"]), "true");
        // An undecidable condition is not true, and the retry does not make it
        // true either: `at-rule(@font-face)` needs a font backend this tree
        // does not have, so the honest answer is `false`.
        assert_eq!(supports(&["at-rule(@font-face)"]), "false");
        assert_eq!(supports(&["at-rule(@media)"]), "true");
    }
}
