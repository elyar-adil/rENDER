//! Capability-surface audit: print every property name the engine actually
//! installs, so a name claimed by a table can be checked against reality.
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p render-js --example surface_audit
//! ```
//!
//! Two passes, because neither alone is complete:
//!
//! 1. a graph walk from `globalThis` over own properties, prototype chains and
//!    `constructor`, which finds every installed interface and namespace;
//! 2. an explicit instantiation pass over one representative instance of each
//!    installed interface, which finds the methods that only exist on
//!    instances (`next`, `then`, `setUint8`, `getEntries`, ...) and are
//!    invisible to a walk that never calls a constructor.
//!
//! Two sections are printed, separated by `---`: every name reached, then every
//! name whose value is callable. A table that lists a name in neither section
//! is a claim nothing backs.

use render_html::parse_document;
use render_js::{JsRuntime, JsValue};

const WALK: &str = r#"
(function () {
  var seen = [];
  var names = {};
  var callable = {};
  function note(key, value) {
    if (typeof key !== "string") { return; }
    names[key] = true;
    if (value !== null && value !== undefined && typeof value === "function") {
      callable[key] = true;
    }
  }
  function visit(value, depth) {
    if (value === null || value === undefined) { return; }
    var type = typeof value;
    if (type !== "object" && type !== "function") { return; }
    if (seen.indexOf(value) !== -1) { return; }
    seen.push(value);
    if (depth > 10) { return; }
    var keys;
    try { keys = Object.getOwnPropertyNames(value); } catch (e) { return; }
    for (var i = 0; i < keys.length; i++) {
      var key = keys[i];
      var child;
      try { child = value[key]; } catch (e) { child = undefined; }
      note(key, child);
      visit(child, depth + 1);
    }
    var symbols;
    try { symbols = Object.getOwnPropertySymbols(value); } catch (e) { symbols = []; }
    for (var j = 0; j < symbols.length; j++) {
      var symbolKey = symbols[j];
      var described;
      try { described = String(symbolKey); } catch (e) { described = "symbol"; }
      var symbolChild;
      try { symbolChild = value[symbolKey]; } catch (e) { symbolChild = undefined; }
      note(described, symbolChild);
      visit(symbolChild, depth + 1);
    }
    var proto = null;
    try { proto = Object.getPrototypeOf(value); } catch (e) { proto = null; }
    visit(proto, depth + 1);
  }

  // Pass 1: the installed graph.
  visit(globalThis, 0);

  // Pass 2: one live instance of every interface the engine installs, so the
  // instance-only methods are on the record too.
  function attempt(label, make) {
    try { visit(make(), 0); } catch (e) { names["!" + label] = true; }
  }
  attempt("array", function () { return []; });
  attempt("arrayIterator", function () { return [][Symbol.iterator](); });
  attempt("arrayIteratorProto", function () {
    return Object.getPrototypeOf([][Symbol.iterator]());
  });
  attempt("iteratorHelper", function () { return [][Symbol.iterator]().map(function () {}); });
  attempt("iteratorHelperProto", function () {
    return Object.getPrototypeOf([][Symbol.iterator]().map(function () {}));
  });
  attempt("iteratorProto", function () { return Object.getPrototypeOf([][Symbol.iterator]()); });
  attempt("string", function () { return "s"; });
  attempt("number", function () { return 1; });
  attempt("boolean", function () { return true; });
  attempt("symbol", function () { return Symbol("s"); });
  attempt("regexp", function () { return /x/g; });
  attempt("date", function () { return new Date(0); });
  attempt("error", function () { return new Error("e"); });
  attempt("typeError", function () { return new TypeError("e"); });
  attempt("promise", function () { return Promise.resolve(1); });
  attempt("promiseProto", function () { return Promise.prototype; });
  attempt("map", function () { return new Map(); });
  attempt("set", function () { return new Set(); });
  attempt("weakMap", function () { return new WeakMap(); });
  attempt("weakSet", function () { return new WeakSet(); });
  attempt("arrayBuffer", function () { return new ArrayBuffer(4); });
  attempt("dataView", function () { return new DataView(new ArrayBuffer(4)); });
  attempt("typedArray", function () { return new Uint8Array(2); });
  attempt("textEncoder", function () { return new TextEncoder(); });
  attempt("textDecoder", function () { return new TextDecoder(); });
  attempt("url", function () { return new URL("https://example.com/a?b=c#d"); });
  attempt("urlSearchParams", function () { return new URLSearchParams("a=b"); });
  attempt("formData", function () { return new FormData(); });
  attempt("blob", function () { return new Blob([]); });
  attempt("abortController", function () { return new AbortController(); });
  attempt("abortSignal", function () { return new AbortController().signal; });
  attempt("event", function () { return new Event("x"); });
  attempt("xhr", function () { return new XMLHttpRequest(); });
  attempt("response", function () { return new Response("x"); });
  attempt("image", function () { return new Image(); });
  attempt("intersectionObserver", function () { return new IntersectionObserver(function () {}); });
  attempt("mutationObserver", function () { return new MutationObserver(function () {}); });
  attempt("element", function () { return document.createElement("div"); });
  attempt("elementDataset", function () { return document.createElement("div").dataset; });
  attempt("elementStyle", function () { return document.createElement("div").style; });
  attempt("elementClassList", function () { return document.createElement("div").classList; });
  attempt("nodeList", function () { return document.querySelectorAll("div"); });
  attempt("localStorage", function () { return localStorage; });
  attempt("location", function () { return location; });
  attempt("navigator", function () { return navigator; });
  attempt("document", function () { return document; });

  function sorted(map) {
    return Object.keys(map).sort().join("\n");
  }
  return sorted(names) + "\n---\n" + sorted(callable);
})()
"#;

fn as_text(value: &JsValue) -> &str {
    match value {
        JsValue::String(text) => text,
        other => panic!("walk should return a string, got {other:?}"),
    }
}

fn main() {
    let mut parsed = parse_document("<!doctype html><html><body><p>audit</p></body></html>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(&mut parsed.dom, WALK)
        .expect("surface walk should execute");
    let text = as_text(&outcome.value);
    let mut sections = text.split("\n---\n");
    let names = sections.next().unwrap_or_default();
    let callable = sections.next().unwrap_or_default();
    println!("# every name reached ({})", names.lines().count());
    println!("{names}");
    println!(
        "# every callable name reached ({})",
        callable.lines().count()
    );
    println!("{callable}");
}
