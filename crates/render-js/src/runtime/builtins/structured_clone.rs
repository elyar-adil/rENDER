//! `structuredClone` (HTML structured serialize).
//!
//! Frameworks use it to hand state to a worker or to snapshot it, so a missing
//! global is a `ReferenceError` at the point of use. This implements the
//! minimum honest version of the structured serialize algorithm: a deep copy
//! that preserves graph shape, so a cycle in the input is a cycle in the
//! output rather than a stack overflow.
//!
//! **What is covered.** Primitives (including `undefined` and `NaN`), plain
//! objects, arrays, `Map`, `Set`, `Date`, `RegExp`, every typed-array kind, and
//! `ArrayBuffer`. Cycles and repeated references are preserved, because the
//! algorithm memoises by source object rather than walking the graph
//! repeatedly.
//!
//! **What throws, and why that is the point.** A function, a symbol, a promise,
//! and every other host the algorithm lists as non-cloneable raise a
//! `DataCloneError`. Silently dropping them is the failure mode to avoid: code
//! that clones state containing a callback would otherwise get back an object
//! quietly missing that callback, and fail later and further away. A symbol
//! *key* is different: the algorithm copies own enumerable **string**-keyed
//! properties, so a symbol-keyed property is skipped without error, exactly as
//! a browser does.
//!
//! **Known limits, stated rather than hidden.** A DOM node, a `Blob`, a
//! `Storage` area and the other host objects are not cloneable here and throw,
//! which is also what the real algorithm does for a node the embedder cannot
//! transfer. The thrown value is a real `DOMException` named
//! `"DataCloneError"` carrying the legacy `code` 25, so `e instanceof
//! DOMException`, `e.name` and `e.code` all answer - and `e instanceof Error` is
//! true too, because `WebIDL` §3.14.1 hangs `DOMException.prototype` off
//! `%Error.prototype%`.

use std::collections::BTreeMap;

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_exception::DomExceptionName;
use crate::value::{NativeFunction, ObjectHost, TypedArrayKind, TypedBuffer};
use render_dom::Dom;

/// Source object to its copy, which is what makes cycles terminate and repeated
/// references stay shared.
type Memo = BTreeMap<ObjectId, ObjectId>;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_structured_clone_native(
        &mut self,
        _dom: &mut Dom,
        _function: NativeFunction,
        _receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        self.structured_clone_value(&value, &mut Memo::new())
    }

    fn structured_clone_value(
        &mut self,
        value: &JsValue,
        memo: &mut Memo,
    ) -> Result<JsValue, JsError> {
        // Every primitive is transferred by value. A symbol is the one
        // exception: it is not serializable, so it is a `DataCloneError`.
        let object = match value {
            JsValue::Symbol(_) => return Err(self.data_clone_error(value)),
            JsValue::Object(object) => *object,
            other => return Ok(other.clone()),
        };
        if let Some(copied) = memo.get(&object) {
            return Ok(JsValue::Object(*copied));
        }
        match self.realm.host(object) {
            // `Ordinary` is the default host, so it is what a plain object, an
            // object literal, and a prototype-less object all carry. It is the
            // only host with no internal slots of its own, which is exactly the
            // algorithm's "ordinary object" case.
            Some(ObjectHost::Ordinary) => self.clone_plain_object(object, memo),
            Some(ObjectHost::Array) => self.clone_array(object, memo),
            Some(ObjectHost::DateInstance(ms)) => self.clone_date(ms),
            Some(ObjectHost::RegExp(index)) => {
                let Some(compiled) = self.regexes.get(index) else {
                    return Err(self.data_clone_error(value));
                };
                let source = compiled.compiled.source().to_owned();
                let flags = compiled.compiled.flags().describe().clone();
                self.clone_regexp(&source, &flags)
            }
            Some(ObjectHost::Collection { kind, entries }) => {
                // A weak collection is not in the algorithm's serializable set,
                // and its entries are not observable, so it throws rather than
                // cloning as an empty strong collection.
                if kind.is_weak() {
                    return Err(self.data_clone_error(value));
                }
                self.clone_collection(kind, &entries, memo)
            }
            Some(ObjectHost::TypedArray {
                kind,
                buffer,
                start,
                length,
            }) => {
                let size = kind.element_size();
                let bytes = buffer.view_bytes(size, start, length)?;
                let count = bytes.len() / size;
                let prototype = self.typed_array_prototype(kind);
                self.clone_typed_array(kind, &TypedBuffer::new(bytes), count, prototype)
            }
            Some(ObjectHost::ArrayBufferHost(buffer)) => {
                let bytes = buffer.bytes();
                Ok(JsValue::Object(
                    self.array_buffer_object(&TypedBuffer::new(bytes))?,
                ))
            }
            // A callable, a promise, a weak collection, a proxy, and every
            // other host the algorithm lists as non-cloneable.
            Some(_) => Err(self.data_clone_error(value)),
            // No host at all: a boxed primitive such as `Object(1)`, which the
            // algorithm does not serialize, so it is refused.
            None => Err(self.data_clone_error(value)),
        }
    }

    /// Copy an array's own indexed elements, keeping holes as holes and
    /// preserving a repeated reference to the same element object.
    fn clone_array(&mut self, object: ObjectId, memo: &mut Memo) -> Result<JsValue, JsError> {
        let length = self
            .realm
            .get_property(object, "length")
            .map(|value| crate::runtime::builtins::array::to_length(&value))
            .transpose()?
            .unwrap_or(0.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "to_length is a non-negative integer bounded by the array limit"
        )]
        let length = length as usize;
        self.ensure_heap_capacity(1)?;
        let copy = self.realm.create_array();
        // Register before recursing so a self-referential array terminates.
        memo.insert(object, copy);
        for index in 0..length {
            let key = index.to_string();
            let Some(descriptor) = self.realm.own_property(object, &key) else {
                continue;
            };
            let value = self.structured_clone_value(&descriptor.value, memo)?;
            self.realm.set_property(copy, key, value);
        }
        self.realm
            .set_property(copy, "length".to_owned(), JsValue::Number(length as f64));
        Ok(JsValue::Object(copy))
    }

    /// Copy an ordinary object's own enumerable **string**-keyed properties. A
    /// symbol key is skipped without error, which is what the algorithm's
    /// "enumerable own properties" step does.
    fn clone_plain_object(
        &mut self,
        object: ObjectId,
        memo: &mut Memo,
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let copy = self.realm.create_ordinary_object();
        memo.insert(object, copy);
        let properties = self
            .realm
            .enumerable_own_properties(object)
            .unwrap_or_default();
        for (key, value) in properties {
            let cloned = self.structured_clone_value(&value, memo)?;
            self.realm.set_property(copy, key, cloned);
        }
        Ok(JsValue::Object(copy))
    }

    fn clone_date(&mut self, ms: f64) -> Result<JsValue, JsError> {
        let prototype = self.date_prototype();
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(prototype);
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::DateInstance(ms);
        }
        Ok(JsValue::Object(object))
    }

    fn clone_regexp(&mut self, source: &str, flags: &str) -> Result<JsValue, JsError> {
        let object = self.construct_regex(source, flags)?;
        Ok(JsValue::Object(object))
    }

    /// Copy a `Map` or a `Set`. A `Map`'s keys are cloned as well as its
    /// values, so a key that is itself an object is a distinct copy.
    fn clone_collection(
        &mut self,
        kind: crate::value::CollectionKind,
        entries: &[(JsValue, JsValue)],
        memo: &mut Memo,
    ) -> Result<JsValue, JsError> {
        let mut cloned = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            cloned.push((
                self.structured_clone_value(key, memo)?,
                self.structured_clone_value(value, memo)?,
            ));
        }
        let prototype = self.collection_prototype(kind);
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(prototype);
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::Collection {
                kind,
                entries: cloned,
            };
        }
        Ok(JsValue::Object(object))
    }

    fn clone_typed_array(
        &mut self,
        kind: TypedArrayKind,
        buffer: &TypedBuffer,
        length: usize,
        prototype: Option<ObjectId>,
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.typed_array(
            kind,
            buffer.clone(),
            0,
            Some(length),
            prototype,
        )))
    }

    fn typed_array_prototype(&self, kind: TypedArrayKind) -> Option<ObjectId> {
        self.realm
            .global(kind.name())
            .and_then(|value| match value {
                JsValue::Object(constructor) => self.realm.get_property(constructor, "prototype"),
                _ => None,
            })
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
    }

    fn date_prototype(&self) -> Option<ObjectId> {
        self.realm
            .global("Date")
            .and_then(|value| match value {
                JsValue::Object(constructor) => self.realm.get_property(constructor, "prototype"),
                _ => None,
            })
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
    }

    fn collection_prototype(&self, kind: crate::value::CollectionKind) -> Option<ObjectId> {
        let name = match kind {
            crate::value::CollectionKind::Map => "Map",
            crate::value::CollectionKind::Set => "Set",
            crate::value::CollectionKind::WeakMap => "WeakMap",
            crate::value::CollectionKind::WeakSet => "WeakSet",
        };
        self.realm
            .global(name)
            .and_then(|value| match value {
                JsValue::Object(constructor) => self.realm.get_property(constructor, "prototype"),
                _ => None,
            })
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
    }

    /// The algorithm's `DataCloneError`, as the interface a script can name.
    ///
    /// The HTML Structured Cloning Algorithm says "throw a `DataCloneError`
    /// `DOMException`", and that is now a real `DOMException` rather than a
    /// renamed `TypeError`: `e instanceof DOMException` is true, `e.code` is
    /// 25, and `e.name` is still `"DataCloneError"`, so the two idioms that were
    /// previously mutually exclusive both work. `e instanceof Error` was true
    /// before and stays true, because `WebIDL` §3.14.1 hangs
    /// `DOMException.prototype` off `%Error.prototype%`.
    fn data_clone_error(&mut self, value: &JsValue) -> JsError {
        let description = match value {
            JsValue::Symbol(symbol) => symbol.to_display(),
            JsValue::Object(_) => "[object Object]".to_owned(),
            other => other.to_js_string(),
        };
        self.dom_exception(
            DomExceptionName::DataClone,
            format!("{description} could not be cloned."),
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::JsRuntime;
    use render_html::parse_document;

    /// Every expectation in this module was measured against Node v24.13.1
    /// running the same expression, so a divergence is an engine change rather
    /// than a hand-computed guess.
    fn run(source: &str) -> String {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(&mut parsed.dom, source)
            .expect("clone probe executes");
        outcome.value.to_js_string()
    }

    #[test]
    fn a_plain_object_and_an_array_are_copied_deeply() {
        assert_eq!(
            run("JSON.stringify(structuredClone({a: 1, b: {c: 2}}))"),
            "{\"a\":1,\"b\":{\"c\":2}}"
        );
        assert_eq!(
            run("JSON.stringify(structuredClone([1, [2, 3]]))"),
            "[1,[2,3]]"
        );
        // The copy is a distinct object at every level.
        assert_eq!(
            run(r"
                var source = {a: 1, b: {c: 2}};
                var copy = structuredClone(source);
                [copy !== source, copy.b !== source.b, copy.a].join('|')
            "),
            "true|true|1"
        );
        assert_eq!(run("structuredClone(undefined) === undefined"), "true");
        assert_eq!(run("String(structuredClone(NaN))"), "NaN");
        // A getter is invoked and its value copied, as the real algorithm does.
        assert_eq!(
            run(r"
                var source = {get g() { return 1; }};
                Object.keys(structuredClone(source)).join(',')
            "),
            "g"
        );
    }

    #[test]
    fn a_cycle_and_a_repeated_reference_both_survive() {
        assert_eq!(
            run(r"
                var source = {n: 1};
                source.self = source;
                var copy = structuredClone(source);
                copy.self === copy
            "),
            "true"
        );
        // A shared child stays shared in the copy.
        assert_eq!(
            run(r"
                var child = {v: 1};
                var source = {a: child, b: child};
                var copy = structuredClone(source);
                copy.a === copy.b
            "),
            "true"
        );
        assert_eq!(
            run(r"
                var list = [];
                list.push(list);
                var copy = structuredClone(list);
                copy[0] === copy
            "),
            "true"
        );
    }

    #[test]
    fn a_map_set_date_and_regexp_keep_their_brand_and_their_contents() {
        assert_eq!(
            run(r"
                var source = new Map([['k', 1]]);
                var copy = structuredClone(source);
                [copy instanceof Map, copy.get('k'), copy !== source].join('|')
            "),
            "true|1|true"
        );
        assert_eq!(
            run(r"
                var source = new Set([1, 2]);
                var copy = structuredClone(source);
                [copy instanceof Set, copy.has(2), copy !== source].join('|')
            "),
            "true|true|true"
        );
        assert_eq!(
            run(r"
                var source = new Date(1234);
                var copy = structuredClone(source);
                [copy instanceof Date, copy.getTime(), copy !== source].join('|')
            "),
            "true|1234|true"
        );
        assert_eq!(
            run(r"
                var source = /ab+/gi;
                var copy = structuredClone(source);
                [copy instanceof RegExp, copy.source, copy.flags, copy !== source].join('|')
            "),
            "true|ab+|gi|true"
        );
        // A Map key that is itself an object is cloned, so the copy's key is
        // not the source's key but does match on contents.
        assert_eq!(
            run(r"
                var key = {k: 1};
                var copy = structuredClone(new Map([[key, 'v']]));
                var keys = [];
                copy.forEach(function (value, k) { keys.push(k); });
                [keys.length, keys[0] !== key, copy.get(keys[0])].join('|')
            "),
            "1|true|v"
        );
    }

    #[test]
    fn a_typed_array_and_a_buffer_are_copied_byte_for_byte() {
        assert_eq!(
            run(r"
                var source = new Uint8Array([1, 2, 3]);
                var copy = structuredClone(source);
                [copy instanceof Uint8Array, Array.from(copy).join(','), copy !== source].join('|')
            "),
            "true|1,2,3|true"
        );
        assert_eq!(
            run(r"
                var source = new ArrayBuffer(4);
                new Uint8Array(source)[0] = 5;
                var copy = structuredClone(source);
                [Object.prototype.toString.call(copy), new Uint8Array(copy)[0],
                 copy === source].join('|')
            "),
            "[object ArrayBuffer]|5|false"
        );
        // The typed-array copy is independent of its source.
        assert_eq!(
            run(r"
                var source = new Uint8Array([1]);
                var copy = structuredClone(source);
                copy[0] = 9;
                source[0]
            "),
            "1"
        );
    }

    #[test]
    fn a_function_symbol_or_promise_is_a_data_clone_error() {
        // Dropping these silently is the failure this API exists to prevent, so
        // each one throws by name.
        assert_eq!(
            run(r"
                function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                [thrown(function () { return structuredClone({f: function () {}}); }),
                 thrown(function () { return structuredClone(function () {}); }),
                 thrown(function () { return structuredClone(Symbol('s')); }),
                 thrown(function () { return structuredClone(Promise.resolve(1)); }),
                 thrown(function () { return structuredClone(new WeakMap()); }),
                 thrown(function () { return structuredClone(new Proxy({}, {})); })].join(',')
            "),
            "DataCloneError,DataCloneError,DataCloneError,DataCloneError,\
DataCloneError,DataCloneError"
        );
        // The thrown value is a real `DOMException`, so all three of the ways
        // code branches on an error work: `e.name`, `e instanceof DOMException`,
        // and `e instanceof Error` (which is true because WebIDL §3.14.1 hangs
        // `DOMException.prototype` off `%Error.prototype%`). Before this
        // interface existed the middle one was false for every Web API error in
        // the engine, which is the idiom `catch (e) { if (e instanceof
        // DOMException) ... }` depends on.
        assert_eq!(
            run(r"
                try { structuredClone(function () {}); } catch (e) {
                    [e instanceof DOMException, e instanceof Error, e.name, e.code,
                     String(e).split(':')[0]].join(',');
                }
            "),
            "true,true,DataCloneError,25,DataCloneError"
        );
        // A symbol *key* is skipped without error: the algorithm copies own
        // enumerable string-keyed properties.
        assert_eq!(
            run("JSON.stringify(structuredClone({[Symbol('k')]: 1, a: 2}))"),
            "{\"a\":2}"
        );
    }

    #[test]
    fn a_host_the_engine_does_not_clone_throws_rather_than_copying() {
        assert_eq!(
            run(r"
                function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                thrown(function () { return structuredClone(document); })
            "),
            "DataCloneError"
        );
    }
}
