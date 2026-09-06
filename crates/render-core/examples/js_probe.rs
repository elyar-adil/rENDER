use render_core::html::parse_document;
use render_core::js::JsRuntime;

fn main() {
    let child = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(run)
        .expect("spawn probe thread");
    child.join().expect("probe panicked");
}

#[allow(
    clippy::too_many_lines,
    reason = "diagnostic probe lists many snippets"
)]
fn run() {
    let builtin = [
        ("var-init", r"var x = 5; x"),
        (
            "closure-var-capture",
            r#"(function(window){ var document = window.document, tag="t"; var inner = function(){ return (document === window.document) + ":" + tag; }; return inner(); })(this)"#,
        ),
        (
            "chained-assign",
            r#"var o = {}; o.a = o.b = {v:1}; (!!o.a) + ":" + (!!o.b)"#,
        ),
        (
            "fn-prop-chain",
            "function C(){}; C.prototype = {x:1}; C.fn = C.prototype; typeof C.fn.x",
        ),
        (
            "jquery-init-shape",
            r#"function Ctor(sel){ this.tag="outer"; if(!sel){ return this; } return new Ctor.fn.init(sel); }
               Ctor.fn = Ctor.prototype = { init: function(sel){ if(!sel){ return this; } return new Ctor.fn.init(sel); } };
               Ctor.fn.init.prototype = Ctor.fn;
               var a = new Ctor.fn.init("");
               var b = Ctor("x");
               (a instanceof Ctor.fn.init) + ":" + (b.tag === "outer");"#,
        ),
        ("fn-var", r"function f(){ var y = 1; return y; } f()"),
        (
            "recursive-declaration",
            "function factorial(n){ if (n <= 1) { return 1; } return n * factorial(n - 1); } factorial(5)",
        ),
        (
            "recursive-var-binding",
            "var f2 = function(n){ if (n <= 1) { return 1; } return n * f2(n - 1); }; f2(5)",
        ),
        (
            "one-level-recursion",
            "function g(n){ if (n <= 1) { return 1; } return g(n - 1); } g(2)",
        ),
        (
            "typeof-recursive-name",
            "function g(n){ return typeof g; } g(2)",
        ),
        (
            "arith-multiply",
            "function g(n){ return n * (n - 1); } g(5)",
        ),
        ("param-arithmetic", "function g(n){ return n - 1; } g(5)"),
        (
            "two-call-recursion",
            "function g(n){ if (n <= 1) { return 1; } return g(n - 1) * 2; } g(3)",
        ),
        (
            "depth3-no-multiply",
            "function g(n){ if (n <= 1) { return 1; } return g(n - 1); } g(3)",
        ),
        (
            "depth3-var-then-return",
            "function g(n){ if (n <= 1) { return 1; } var r = g(n - 1); return r; } g(3)",
        ),
        (
            "depth3-plus-zero",
            "function g(n){ if (n <= 1) { return 1; } return g(n - 1) + 0; } g(3)",
        ),
        (
            "nested-call-arg",
            "function id(n){ return n; } function f(n){ return id(id(n)); } f(5)",
        ),
        (
            "g1-with-literal",
            "function g(n){ if (n <= 1) { return 1; } return g(0); } g(3)",
        ),
        (
            "selector-regression",
            r##"const root = document.querySelector("#app");
               const cards = root.querySelectorAll("article.card");
               cards[0].classList.add("selected", "visible");
               cards[1].classList.toggle("card", false);
               const badge = document.createElement("span");
               badge.className = "badge";
               badge.textContent = "ok";
               root.insertBefore(badge, cards[0]);
               const selected = root.querySelector(".selected");
               const c1 = selected.classList.contains("visible");
               const c2 = selected.classList.item(0) === "card";
               const c3 = selected.classList.toString() === "card first selected visible";
               const c4 = root.querySelectorAll(".card").length === 1;
               const c5 = root.firstChild.className === "badge";
               const c6 = root.textContent === "okonetwo";
               [c1,c2,c3,c4,c5,c6].join(",");"##,
        ),
        (
            "fn-var-list",
            r#"function g(){ var a, b = 2; return [a, b].join(","); } g()"#,
        ),
        (
            "hoist-read",
            r"function h(){ return z; var z = 9; } h() === undefined",
        ),
        (
            "jquery-var-shape",
            r#"(function(window, undefined){ var readyList, rootjQuery, core_strundefined = typeof undefined, document = window.document; return core_strundefined + ":" + (document === window.document); })(this)"#,
        ),
        ("in-after-string", r#"var l = {flags: 1}; "flags" in l"#),
        (
            "in-nested-parens",
            r#"var p = true, l = {flags: 1}, e = {}, t; (e = e.source, p && (t = "flags" in l ? l.flags : 2)); t"#,
        ),
        (
            "multi-elision",
            r#"var m = [function(){return 1;},,,,function(){return 2;}]; m.length + ":" + (m[0]() + m[4]())"#,
        ),
        (
            "elision",
            r#"var m = [function(){return 1;},,[,],function(){return 2;}]; m.length + ":" + (m[0]() + m[3]())"#,
        ),
        // Regression: a regex literal may follow `return` and a block-closing
        // `}`; the lexer must not read it as a division operator.
        (
            "regex-after-return-brace",
            r#"var f=function(e){return};/^http(s)?:/.test("https:") + ":" + (/^\w+:\/\//.test("mailto:"))"#,
        ),
        (
            "regex-after-object-brace-division",
            r"var x = 8 / {v:2}.v; x",
        ),
        // Regression (zhihu main.app.js byte 974425): a regex literal may
        // directly follow the `}` that closes a function body after a bare
        // `return`, with no intervening semicolon.
        (
            "regex-after-bare-return-brace",
            r#"var hit = "";
               var f = function(e){
                   if (e) { return }
                   /^http(s)?:/.test(e) && (hit = "regex");
               };
               f(""); hit"#,
        ),
        // Regression: a UTF-8 byte-order mark (U+FEFF) is WhiteSpace per
        // ECMA-262 and must not abort tokenization.
        ("bom-whitespace", "\u{feff}var x = 7; x"),
        // Typed-array family probes.
        (
            "typed-array-u16-react-shape",
            r#"var n = new Uint16Array(16); for (var O = 0; 15 > O; O++) n[O] = O + 1; n[15] = 0;
               var grown = new Uint16Array(32); grown.set(n);
               var first = n[0], len = n.length, bytes = Uint16Array.BYTES_PER_ELEMENT;
               var thread = n[1]; n[0] = n[thread]; n[1] = 0;
               [first, len, bytes, grown[1], n[0], n[1]].join(",");"#,
        ),
        (
            "typed-array-encode-rules",
            r#"var u8 = new Uint8Array(3); u8[0] = 300; u8[1] = -1; u8[2] = 255.7;
               var i8 = new Int8Array(2); i8[0] = 128; i8[1] = -129;
               var uc = new Uint8ClampedArray(3); uc[0] = -5; uc[1] = 300; uc[2] = 127.5;
               var u16 = new Uint16Array(1); u16[0] = 65536;
               var f32 = new Float32Array(1); f32[0] = 0.1;
               [u8[0], u8[1], u8[2], i8[0], i8[1], uc[0], uc[1], uc[2], u16[0], f32[0]].join(",");"#,
        ),
        (
            "typed-array-methods",
            r#"var a = new Int32Array(4); a.set([1, 2, 3], 1); a.fill(9, 2, 3);
               var sub = a.subarray(1, 3); sub[0] = 40;
               var copy = a.slice(1, 3); copy[0] = 77;
               var idx = a.indexOf(9);
               var joined = a.join("|");
               var iterated = ""; for (var v of a) iterated += v + ";";
               [a[1], sub[0], copy[0], idx, joined, iterated].join(",");"#,
        ),
        (
            "typed-array-identity",
            r#"var t = new Uint16Array(4); var e = t[99]; var tag = Object.prototype.toString.call(t);
               var sep = Uint8Array === Uint16Array; var ok = t instanceof Uint16Array;
               [e, tag, sep, ok, typeof t].join(",");"#,
        ),
        (
            "typed-array-from",
            r#"var a = Uint8ClampedArray.from([1, 2, 3]);
               var doubled = Uint16Array.from([1, 2, 3], function (v) { return v * 2; });
               var text = Uint8Array.from("AZ");
               [a[1], a.length, doubled[2], text[0], text[1]].join(",");"#,
        ),
        (
            "typed-array-clamp-assign",
            r#"var a = new Uint8ClampedArray(1); a[0] = 300; var hi = a[0]; a[0] = -1; var lo = a[0];
               var b = new Int8Array(1); b[0] = 200; var wrapped = b[0];
               var seen = ""; var c = new Uint8Array([5, 250]);
               c.forEach(function (v, i) { seen += i + ":" + v + ";"; });
               [hi, lo, wrapped, seen].join(",");"#,
        ),
        (
            "typed-array-callbacks",
            r#"var a = new Int16Array([1, 2, 3, 4]);
               var doubled = a.map(function (v) { return v * 2; });
               var evens = a.filter(function (v) { return v % 2 === 0; });
               [doubled.constructor === Int16Array, doubled[2], evens[1], evens.length,
                a.includes(3), a.includes(9)].join(",");"#,
        ),
        // Deep-recursion probe: must complete, and runaway recursion must
        // surface a RangeError rather than a silent failure.
        (
            "deep-recursion-bounded",
            r"function f(n){ if(n<=0) return 0; return f(n-1)+1; } f(4000)",
        ),
        (
            "deep-recursion-range-error",
            r#"var kind = ""; try { function f(n){ return f(n+1); } f(0); } catch (e) { kind = e.name + ":" + (e instanceof RangeError); } kind"#,
        ),
    ];
    for (name, source) in builtin {
        let document = if name == "selector-regression" {
            "<!doctype html><main id='app'><article class='card first'>one</article><article class='card'>two</article></main>"
        } else {
            "<!doctype html><p></p>"
        };
        let mut parsed = parse_document(document);
        let mut runtime = JsRuntime::new(&parsed.dom);
        match runtime.execute(&mut parsed.dom, source) {
            Ok(outcome) => println!("OK   {name}: {:?}", outcome.value),
            Err(error) => println!("ERR  {name}: {}", error.message()),
        }
    }
    let mut target_document = parse_document("<!doctype html><p></p>");
    let mut target_runtime = JsRuntime::new(&target_document.dom);
    for path in std::env::args().skip(1) {
        let Ok(source) = std::fs::read_to_string(&path) else {
            println!("SKIP {path} (unreadable)");
            continue;
        };
        let name = std::path::Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&path)
            .to_owned();
        match target_runtime.execute(&mut target_document.dom, &source) {
            Ok(outcome) => println!("OK   {name}: {:?}", outcome.value),
            Err(error) => {
                println!("ERR  {name}: {} at {:?}", error.message(), error.offset());
                if let Some(offset) = error.offset() {
                    let start = offset.saturating_sub(25);
                    let end = (offset + 25).min(source.len());
                    println!(
                        "     ...{}[{}]{}...",
                        &source[start..offset],
                        &source[offset..(offset + 1).min(source.len())],
                        &source[(offset + 1).min(source.len())..end]
                    );
                }
            }
        }
    }
}
