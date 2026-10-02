//! `DOMException` (`WebIDL` §4.4) and `WebIDL` §3.14.3's "create a `DOMException`
//! given a string name".
//!
//! The interface itself installs in `value.rs` next to the ECMAScript error
//! hierarchy, because its heritage is a statement about `%Error.prototype%`
//! and about nothing else. This file holds the parts that need the runtime: the
//! constructor, the three attribute accessors, and the one helper every
//! migrated Web API error site calls.
//!
//! ## Why the helper exists rather than an enum of error kinds
//!
//! `JsErrorKind` is a six-variant classification of *this crate's* failures, and
//! it is the wrong level of detail for a Web API error: it says "this is a DOM
//! error", not "this is a `NotFoundError`", and `e.name` is the only thing that
//! tells a script which of the two it got. Before this interface existed the
//! engine had nowhere to put that answer, so the sites that had one set `name` on
//! a plain `TypeError` and left `instanceof DOMException` false. Both halves of
//! the idiom were wrong for every Web API error in the engine, and
//! `catch (e) { if (e instanceof DOMException) ... }` - the form libraries
//! actually write - took the wrong branch for all of them.

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::PropertyDescriptor;
use crate::value::dom_exception_code;

/// A `name` this engine *throws*.
///
/// The enum is deliberately not `WebIDL` §2.8.1's whole names table. That table
/// is in `crate::value` as `DOM_EXCEPTION_NAME_CODES`, where it belongs: the
/// `code` getter has to consult it, because `new DOMException("m", name)` is
/// script-reachable and every name in the table has to produce its code. This
/// enum is the opposite thing: it is the set of names an engine *throw site* can
/// produce, so a variant is a claim that some API in this crate throws it and an
/// unused variant is a claim nothing backs. That is the same defect
/// `builtin_arity` was found to have, and the same fix: keep the list to what is
/// real and let the compiler police additions.
///
/// §2.8.1's rows without a legacy code are not here for the same reason - nothing
/// in this crate throws them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runtime) enum DomExceptionName {
    /// `querySelector` and friends, on a selector the parser rejects.
    ///
    /// Not JavaScript's `SyntaxError`, and §2.8.1 is explicit about why: "this
    /// name is used to report parsing errors in web APIs, for example when
    /// parsing selectors, while the JavaScript `SyntaxError` is reserved for the
    /// JavaScript parser." So `e.name` is `"SyntaxError"` and
    /// `e instanceof SyntaxError` is **false**.
    Syntax,
    /// `atob`/`btoa` on a non-base64 or non-Latin-1 character, and
    /// `createElement`/`DOMTokenList` on a name no element or token can have.
    InvalidCharacter,
    /// `outerHTML` on a node with no parent to splice into.
    NoModificationAllowed,
    /// `cloneNode` on a node the algorithm cannot copy, and a `play()` with
    /// nothing playable to point at.
    NotSupported,
    /// `structuredClone` on a value the algorithm refuses to copy.
    DataClone,
    /// `fetch`/`XMLHttpRequest` cancelled through an `AbortSignal`, and a
    /// `load()` that supersedes an in-flight media load.
    Abort,
    /// The Encoding Standard's fatal-decode throw, which says "throw a
    /// `TypeError`" - and a Web API specification that says that means a
    /// `DOMException` *named* `TypeError`, not the ECMAScript error. §4.4's code
    /// getter gives `0` for a name with no row in §2.8.1's table, and that is
    /// the correct answer rather than a missing one.
    EcmascriptTypeError,
    /// The Encoding Standard's unknown-label throw, for the same reason as
    /// [`Self::EcmascriptTypeError`].
    EcmascriptRangeError,
}

impl DomExceptionName {
    pub(in crate::runtime) const fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "SyntaxError",
            Self::InvalidCharacter => "InvalidCharacterError",
            Self::NoModificationAllowed => "NoModificationAllowedError",
            Self::NotSupported => "NotSupportedError",
            Self::DataClone => "DataCloneError",
            Self::Abort => "AbortError",
            Self::EcmascriptTypeError => "TypeError",
            Self::EcmascriptRangeError => "RangeError",
        }
    }
}

impl JsRuntime {
    /// `new DOMException(message = "", name = "Error")` (`WebIDL` §4.4).
    ///
    /// The message comes first and the name second, which is the reverse of how
    /// a `DOMException` reads in prose and the reverse of what a `catch` clause
    /// tends to assume. Both are optional and both are `DOMString`, so the only
    /// step is the two assignments: there is no coercion beyond
    /// `ToString` and no validation, because §4.4 validates nothing - a caller
    /// may pass a name that is not in the table and the object reports it
    /// verbatim with a `code` of `0`.
    pub(in crate::runtime) fn dom_exception_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let message = arguments
            .first()
            .map_or_else(String::new, JsValue::to_js_string);
        let name = match arguments.get(1) {
            Some(value) => value.to_js_string(),
            None => "Error".to_owned(),
        };
        let Some(prototype) = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(prototype) => Some(prototype),
                _ => None,
            })
        else {
            return Err(JsError::type_error(
                "DOMException constructor prototype is not an object",
            ));
        };
        self.ensure_heap_capacity(2)?;
        let object = self.realm.create_object(Some(prototype));
        *self
            .realm
            .host_mut(object)
            .expect("a freshly created object has host storage") =
            ObjectHost::DomException { name, message };
        self.install_exception_stack(object);
        Ok(JsValue::Object(object))
    }

    /// The `name`, `message` and `code` accessors of `DOMException.prototype`.
    ///
    /// A `readonly attribute` is an accessor on the prototype reading an
    /// internal slot, so the values are not own properties of the instance and a
    /// write to one is refused rather than shadowing the slot. `code` is
    /// computed, not stored: §4.4 defines it as a lookup of the name in §2.8.1's
    /// table, and storing it would create a second source of truth for a value
    /// that is a pure function of the first.
    pub(in crate::runtime) fn dom_exception_accessor(
        &self,
        receiver: ObjectId,
        function: NativeFunction,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::DomException { name, message }) = self.realm.host(receiver) else {
            return Err(JsError::type_error(
                "DOMException.prototype accessor called on an incompatible receiver",
            ));
        };
        Ok(match function {
            NativeFunction::DomExceptionNameGetter => JsValue::String(name),
            NativeFunction::DomExceptionMessageGetter => JsValue::String(message),
            NativeFunction::DomExceptionCodeGetter => {
                JsValue::Number(f64::from(dom_exception_code(&name)))
            }
            _ => {
                return Err(JsError::type_error(
                    "unrecognized DOMException.prototype accessor",
                ));
            }
        })
    }

    /// `WebIDL` §3.14.3, "create a `DOMException` given a string name": one new
    /// `DOMException` in the current realm with the given name and an
    /// implementation-defined message.
    ///
    /// This is the *throwing* site's entry point rather than
    /// `new DOMException(...)` on purpose. §3.14.3's "To create a `DOMException`
    /// given a string name" is a two-step: allocate the exception, then set its
    /// message, and a specification that says "throw a `NotFoundError` `DOMException`
    /// with message *m*" means exactly that - not "construct one with
    /// (`*m*`, `"NotFoundError"`)" as an argument order, and not "construct an
    /// `Error` and assign `name`". Going through the interface object would
    /// produce the same object here, but only because this engine's
    /// `DOMException` takes both strings; the distinction matters for the
    /// `[[Stack]]` slot, which the algorithm fills at creation.
    pub(in crate::runtime) fn construct_dom_exception(
        &mut self,
        name: DomExceptionName,
        message: &str,
    ) -> Result<JsValue, JsError> {
        let Some(JsValue::Object(constructor)) = self.realm.global("DOMException") else {
            // `DOMException` is installed unconditionally by `bootstrap`, so this
            // arm is unreachable; answering with a plain `Error` rather than
            // panicking keeps a resource-exhausted realm from taking the process
            // down over a diagnostic.
            return self
                .construct_standard_error(crate::value::ErrorKind::Error, message)
                .map_err(|_| JsError::resource(message.to_owned()));
        };
        let arguments = [
            JsValue::String(message.to_owned()),
            JsValue::String(name.as_str().to_owned()),
        ];
        self.dom_exception_constructor(constructor, &arguments)
    }

    /// The form every Web API error site in this crate uses: build the
    /// `DOMException` the specification names and wrap it as a thrown value.
    pub(in crate::runtime) fn dom_exception(
        &mut self,
        name: DomExceptionName,
        message: impl Into<String>,
    ) -> JsError {
        let message = message.into();
        match self.construct_dom_exception(name, &message) {
            Ok(value) => JsError::thrown(value),
            // No realm to construct in. A string is the weakest answer that
            // still carries the name, because a bare `JsError` string loses
            // `e.name` and that is the member the whole migration is about.
            Err(_) => JsError::thrown(JsValue::String(format!("{}: {message}", name.as_str()))),
        }
    }

    /// Attach the `[[Stack]]` slot as the own `stack` property the ECMAScript
    /// error constructor exposes, so `e.stack` reads on a `DOMException` the way
    /// it reads on an `Error`. `WebIDL` §3.14.1 gives `DOMException` the slot
    /// "like all built-in exceptions"; where it is published is this engine's
    /// choice, and it already publishes `Error.prototype.stack`'s value the same
    /// way.
    fn install_exception_stack(&mut self, object: ObjectId) {
        let header = match self.realm.host(object) {
            Some(ObjectHost::DomException { name, message }) => {
                if message.is_empty() {
                    name
                } else {
                    format!("{name}: {message}")
                }
            }
            _ => self
                .realm
                .get_property(object, "name")
                .unwrap_or_else(|| JsValue::String("Error".to_owned()))
                .to_js_string(),
        };
        let stack = format!("{header}{}", self.stack_frame_lines());
        self.realm.define_property(
            object,
            "stack",
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::String(stack),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::DomExceptionName;
    use crate::JsValue;
    use crate::runtime::JsRuntime;
    use crate::value::dom_exception_code;
    use render_html::parse_document;

    fn run(source: &str) -> String {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        match runtime.execute(&mut parsed.dom, source) {
            Ok(outcome) => outcome.value.to_js_string(),
            Err(error) => format!("<threw {}>", error.message()),
        }
    }

    /// The two halves of `WebIDL` §3.14.1 at once, because they are the pair that
    /// is easy to get wrong in either direction.
    ///
    /// `DOMException.prototype`'s `[[Prototype]]` is `%Error.prototype%`, so
    /// `instanceof Error` is **true** - which is the whole reason for the
    /// override, since `catch (e) { if (e instanceof Error) ... }` is the check
    /// real code writes when it wants to know whether it caught an exception at
    /// all. The IDL fragment declares no inheritance clause, so in the
    /// specification's own type system a `DOMException` is not an `Error`; what
    /// the binding does is hang the prototype off `%Error.prototype%` and give
    /// the object `[[ErrorData]]` and `[[Stack]]` "like all built-in exceptions".
    /// Both facts are asserted together because asserting either alone is the
    /// shape of the bug.
    #[test]
    fn a_dom_exception_is_an_error_and_its_own_interface() {
        assert_eq!(
            run(r"
                var e = new DOMException('missing', 'NotFoundError');
                [
                    e instanceof DOMException,
                    e instanceof Error,
                    Object.prototype.toString.call(e),
                    e.name,
                    e.message,
                    e.code
                ].join(',')
            "),
            "true,true,[object DOMException],NotFoundError,missing,8"
        );
    }

    /// The heritage is only half the story: `Error.prototype.toString` is
    /// inherited, so `String(e)` is `"name: message"` and the empty-message
    /// case drops the name the way the spec's `Error.prototype.toString` does.
    #[test]
    fn a_dom_exception_stringifies_through_error_prototype_to_string() {
        assert_eq!(
            run("String(new DOMException('m', 'NotFoundError'))"),
            "NotFoundError: m"
        );
        assert_eq!(
            run("String(new DOMException('', 'NotFoundError'))"),
            "NotFoundError"
        );
        assert_eq!(run("String(new DOMException('m', ''))"), "m");
        assert_eq!(run("new DOMException().toString()"), "Error");
        // The default name is `Error` and the default message is empty
        // (WebIDL §4.4's constructor signature).
        assert_eq!(
            run("[new DOMException().name, new DOMException().message].join('|')"),
            "Error|"
        );
        // `[[Stack]]` is present, because §3.14.1 gives it one "like all built-in
        // exceptions", and it carries the same header `Error`'s does.
        assert_eq!(
            run("typeof new DOMException('m', 'NotFoundError').stack"),
            "string"
        );
        assert_eq!(
            run("new DOMException('m', 'NotFoundError').stack.split('\\n')[0]"),
            "NotFoundError: m"
        );
    }

    /// `name`, `message` and `code` are `WebIDL` §2.5.2 readonly attributes:
    /// accessors on the prototype reading internal slots. So they are not own
    /// properties of the instance - `Object.getOwnPropertyNames` on one lists
    /// only `stack` - and a write to one is refused instead of quietly
    /// shadowing the slot.
    #[test]
    fn the_three_attributes_are_prototype_accessors_not_own_properties() {
        assert_eq!(
            run("Object.getOwnPropertyNames(new DOMException('m', 'NotFoundError')).join(',')"),
            "stack"
        );
        assert_eq!(
            run("Object.keys(new DOMException('m', 'NotFoundError')).length"),
            "0"
        );
        // A strict-mode write to a getter-only accessor throws; the engine
        // reports the *attempted* value unchanged either way, which is the part
        // a caller can observe.
        assert_eq!(
            run(
                "'use strict'; var e = new DOMException('m', 'NotFoundError'); e.name = 'x'; e.name"
            ),
            "NotFoundError"
        );
        assert_eq!(
            run(
                "var d = Object.getOwnPropertyDescriptor(DOMException.prototype, 'code'); \
                [typeof d.get, typeof d.set, d.enumerable, d.configurable].join(',')"
            ),
            "function,undefined,false,true"
        );
    }

    /// All 25 legacy constants of `WebIDL` §4.4, on the interface object *and* on
    /// the prototype, with the values the IDL gives them.
    #[test]
    fn the_legacy_code_constants_are_installed_on_the_constructor_and_the_prototype() {
        assert_eq!(
            run(r"
                var names = ['INDEX_SIZE_ERR','DOMSTRING_SIZE_ERR','HIERARCHY_REQUEST_ERR',
                    'WRONG_DOCUMENT_ERR','INVALID_CHARACTER_ERR','NO_DATA_ALLOWED_ERR',
                    'NO_MODIFICATION_ALLOWED_ERR','NOT_FOUND_ERR','NOT_SUPPORTED_ERR',
                    'INUSE_ATTRIBUTE_ERR','INVALID_STATE_ERR','SYNTAX_ERR',
                    'INVALID_MODIFICATION_ERR','NAMESPACE_ERR','INVALID_ACCESS_ERR',
                    'VALIDATION_ERR','TYPE_MISMATCH_ERR','SECURITY_ERR','NETWORK_ERR',
                    'ABORT_ERR','URL_MISMATCH_ERR','QUOTA_EXCEEDED_ERR','TIMEOUT_ERR',
                    'INVALID_NODE_TYPE_ERR','DATA_CLONE_ERR'];
                var mismatched = names.filter(function (n) {
                    return DOMException[n] !== DOMException.prototype[n];
                }).length;
                [names.length, mismatched, DOMException.NOT_FOUND_ERR,
                 DOMException.DATA_CLONE_ERR, DOMException.DOMSTRING_SIZE_ERR].join(',')
            "),
            "25,0,8,25,2"
        );
        // A constant is `enumerable` and neither writable nor configurable
        // (WebIDL §2.5.1: "the constant value can be accessed ... as
        // `A.rambaldi` or `instanceOfA.rambaldi`").
        assert_eq!(
            run(
                "var d = Object.getOwnPropertyDescriptor(DOMException, 'NOT_FOUND_ERR'); \
                [d.value, d.writable, d.enumerable, d.configurable].join(',')"
            ),
            "8,false,true,false"
        );
    }

    /// §4.4's code getter is a lookup of the *name* in §2.8.1's table, and the
    /// table and the constants are not the same list. Three consequences, each
    /// of which a `code` implemented as "the constant of the same name" gets
    /// wrong:
    ///
    /// - a name with no table row reports `0`, including the eleven §2.8.1 rows
    ///   that carry no code and any name the caller invented;
    /// - a name that is an ECMAScript error name (`"TypeError"`,
    ///   `"RangeError"`) is what a Web API specification means when it says
    ///   "throw a `TypeError`", and it reports `0`;
    /// - the constructor validates nothing, so an unknown name is kept verbatim.
    #[test]
    fn code_is_a_lookup_of_the_name_not_a_copy_of_the_constants() {
        assert_eq!(run("new DOMException('m', 'EncodingError').code"), "0");
        assert_eq!(run("new DOMException('m', 'NotAllowedError').code"), "0");
        assert_eq!(run("new DOMException('m', 'TypeError').code"), "0");
        assert_eq!(run("new DOMException('m', 'RangeError').code"), "0");
        assert_eq!(run("new DOMException('m', 'Nonesuch').code"), "0");
        // The name is kept verbatim rather than normalised away.
        assert_eq!(run("new DOMException('m', 'Nonesuch').name"), "Nonesuch");
        // The rows the table *does* carry, including the deprecated ones and
        // the ones with no `*_ERR` constant at all.
        assert_eq!(run("new DOMException('m','IndexSizeError').code"), "1");
        assert_eq!(
            run("new DOMException('m','NoModificationAllowedError').code"),
            "7"
        );
        assert_eq!(
            run("new DOMException('m','InUseAttributeError').code"),
            "10"
        );
        assert_eq!(run("new DOMException('m','TypeMismatchError').code"), "17");
        assert_eq!(run("new DOMException('m','QuotaExceededError').code"), "22");
        assert_eq!(
            run("new DOMException('m','InvalidNodeTypeError').code"),
            "24"
        );
    }

    /// The name table and the enum cannot drift: every name this engine can
    /// throw is a row of `WebIDL` §2.8.1 with the code that row carries, or one of
    /// the two ECMAScript-named rows with no code at all.
    #[test]
    fn every_name_this_engine_throws_reads_its_code_from_the_spec_table() {
        for (name, expected) in [
            (DomExceptionName::Syntax, 12),
            (DomExceptionName::InvalidCharacter, 5),
            (DomExceptionName::NoModificationAllowed, 7),
            (DomExceptionName::NotSupported, 9),
            (DomExceptionName::DataClone, 25),
            (DomExceptionName::Abort, 20),
            (DomExceptionName::EcmascriptTypeError, 0),
            (DomExceptionName::EcmascriptRangeError, 0),
        ] {
            assert_eq!(
                dom_exception_code(name.as_str()),
                expected,
                "{} should read code {expected}",
                name.as_str()
            );
        }
    }

    /// The migration itself: every Web API error the engine raises is now a real
    /// `DOMException`. `e.name` is unchanged by that - which is why a caller that
    /// only reads `name` sees nothing - and `e instanceof DOMException` changed
    /// from false to true, which is why the caller that reads *that* now works.
    ///
    /// The five APIs asserted here are the ones that take a selector. The parse
    /// is one shared step for all of them, so the exception is the same one.
    #[test]
    fn a_bad_selector_throws_a_dom_exception_named_syntax_error() {
        assert_eq!(
            run(r"
                function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                [thrown(function () { return document.querySelector('::'); }),
                 thrown(function () { return document.querySelectorAll('a b c d e ! !'); }),
                 thrown(function () { return document.body.matches('&&'); }),
                 thrown(function () { return document.getElementsByClassName('a!'); }),
                 thrown(function () { return document.body.closest('&&'); })].join(',')
            "),
            "SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError"
        );
        // The `SyntaxError` DOMException is not JavaScript's `SyntaxError`
        // (WebIDL §2.8.1: "this name is used to report parsing errors in web
        // APIs, for example when parsing selectors, while the JavaScript
        // SyntaxError is reserved for the JavaScript parser"), and it is an
        // `Error` and a `DOMException`.
        assert_eq!(
            run(r"
                try { document.querySelector('::'); } catch (e) {
                    [e instanceof DOMException, e instanceof Error, e instanceof SyntaxError, e.code].join(',');
                }
            "),
            "true,true,false,12"
        );
        // A selector the parser accepts still does not throw, so the exception
        // is the answer for an invalid selector and not a blanket failure.
        assert_eq!(run("document.querySelector('div p') === null"), "true");
    }

    #[test]
    fn the_other_migrated_web_api_errors_carry_their_specified_names() {
        assert_eq!(
            run(r"
                function name(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name + '/' + e.code; } }
                [
                  name(function () { return atob('a'); }),
                  name(function () { return btoa('\u{1f600}'); }),
                  name(function () { return document.createElement('1bad'); }),
                  name(function () { return document.body.classList.add('a b'); }),
                  name(function () { return structuredClone(function () {}); }),
                  name(function () { return new TextDecoder('utf-16le'); }),
                  name(function () {
                    return new TextDecoder('utf-8', {fatal: true}).decode(new Uint8Array([0xC3]));
                  })
                ].join(' ')
            "),
            "InvalidCharacterError/5 InvalidCharacterError/5 InvalidCharacterError/5 \
InvalidCharacterError/5 DataCloneError/25 RangeError/0 TypeError/0"
        );
    }

    /// A `DOMException` is thrown, not returned, and a `catch` that only knows
    /// `e.name` is unaffected - which is the compatibility claim worth making
    /// about the migration rather than the `instanceof` half.
    #[test]
    fn a_catch_that_reads_only_the_name_is_unaffected_by_the_migration() {
        assert_eq!(
            run(r"
                var seen = 'nothing';
                try { structuredClone(Symbol('s')); } catch (e) { seen = e.name; }
                seen
            "),
            "DataCloneError"
        );
        assert_eq!(
            run(
                "var seen = 'nothing'; try { document.querySelector('::'); } \
                catch (e) { seen = e.name; } seen"
            ),
            "SyntaxError"
        );
    }

    /// The `Video`/`fetch` rejections are the other migrated family, and they
    /// answer through a promise rather than a `throw`, so the reason object has
    /// to be a `DOMException` too - the same `catch`/`then(undefined, e)` code
    /// path, and the same `e instanceof DOMException` check.
    #[test]
    fn a_rejected_media_play_is_a_dom_exception() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r"
                var reason = 'pending';
                new Video().play().then(
                    function () { reason = 'resolved'; },
                    function (e) { reason = [e.name, e instanceof DOMException, e instanceof Error].join('/'); }
                );
                reason
            ",
            )
            .expect("play() should execute");
        assert_eq!(outcome.value, JsValue::String("pending".to_owned()));
        for _ in 0..8 {
            let tasks = runtime.take_pending_microtasks();
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                runtime
                    .invoke_microtask(&mut parsed.dom, task)
                    .expect("the rejection handler should run");
            }
        }
        let result = runtime
            .execute(&mut parsed.dom, "reason")
            .expect("the reason should be readable");
        assert_eq!(result.value.to_js_string(), "NotSupportedError/true/true");
    }
}
