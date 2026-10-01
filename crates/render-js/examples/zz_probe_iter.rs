//! TEMPORARY probe: which builtins expose @@iterator. Deleted before handoff.
use render_html::parse_document;
use render_js::{JsRuntime, JsValue};

const CASES: &[(&str, &str)] = &[
    ("Array.prototype", "typeof Array.prototype[Symbol.iterator]"),
    (
        "String.prototype",
        "typeof String.prototype[Symbol.iterator]",
    ),
    (
        "Map.prototype",
        "typeof Map.prototype[Symbol.iterator]",
    ),
    ("Set.prototype", "typeof Set.prototype[Symbol.iterator]"),
    (
        "Uint8Array.prototype",
        "typeof Uint8Array.prototype[Symbol.iterator]",
    ),
    (
        "Uint8Array instance",
        "typeof new Uint8Array(2)[Symbol.iterator]",
    ),
    (
        "ArrayBuffer.prototype",
        "typeof ArrayBuffer.prototype[Symbol.iterator]",
    ),
    (
        "Map iterator is entries",
        "new Map([[1,2]])[Symbol.iterator]().next().value.join('-')",
    ),
    (
        "Set iterator is values",
        "new Set([3])[Symbol.iterator]().next().value",
    ),
    (
        "Set default iterates values",
        "var s=new Set([3,4]); var a=[]; for (const v of s) a.push(v); a.join(',')",
    ),
    (
        "Map default iterates entries",
        "var m=new Map([[1,2]]); var a=[]; for (const p of m) a.push(p.join(':')); a.join(',')",
    ),
    (
        "ArrayBuffer iterable",
        "var n=0; var b=new ArrayBuffer(4); new Uint8Array(b).set([1,2,3,4]); for (const v of new Uint8Array(b)) n+=v; n",
    ),
    (
        "arraylike object is not iterable",
        "typeof ({length:2})[Symbol.iterator]",
    ),
    (
        "generator object",
        "function* g(){yield 1;} typeof g()[Symbol.iterator]",
    ),
];

fn main() {
    for (label, source) in CASES {
        let mut parsed = parse_document("<!doctype html><p>probe</p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let result = match runtime.execute(&mut parsed.dom, source) {
            Ok(outcome) => {
                let value = outcome.value.clone();
                if matches!(value, JsValue::Undefined) {
                    "undefined".to_owned()
                } else {
                    value.to_js_string()
                }
            }
            Err(error) => format!("ERR {error}"),
        };
        println!("{label:34} {result}");
    }
}