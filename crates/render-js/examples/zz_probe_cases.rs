//! TEMPORARY probe: run each JS snippet in a fresh runtime and print the
//! result. Deleted before handoff.
use render_html::parse_document;
use render_js::{CompiledScript, JsRuntime, RuntimeLimits};

fn main() {
    let mut cases: Vec<(String, String)> = Vec::new();
    cases.push((
        "arr-decl-string".into(),
        "var [x] = 'hi'; x;".into(),
    ));
    cases.push((
        "arr-decl-array".into(),
        "var [a,b] = [1,2]; a+'/'+b;".into(),
    ));
    cases.push(("arr-decl-obj".into(), "var {a} = {a:1}; a;".into()));
    cases.push((
        "arr-decl-rest".into(),
        "var [a,...r] = [1,2,3]; a+'/'+r.join(',');".into(),
    ));
    cases.push((
        "arr-decl-def".into(),
        "var [a,b=9] = [1]; a+'/'+b;".into(),
    ));
    cases.push((
        "arr-decl-hole".into(),
        "var [a,,c] = [1,2,3]; a+'/'+c;".into(),
    ));
    cases.push((
        "obj-decl".into(),
        "var {a} = {a:1}; a;".into(),
    ));
    cases.push((
        "nested".into(),
        "var {a:{b}} = {a:{b:2}}; b;".into(),
    ));
    cases.push((
        "nested-array-in-obj".into(),
        "var {a:[p,q]} = {a:[1,2]}; p+'/'+q;".into(),
    ));
    cases.push((
        "computed".into(),
        "var k='z'; var {[k]:v} = {z:3}; v;".into(),
    ));
    cases.push((
        "obj-rest".into(),
        "var {a,...r} = {a:1,b:2}; a+'/'+r.b;".into(),
    ));
    cases.push((
        "obj-shorthand-default".into(),
        "var {a=5} = {}; a;".into(),
    ));
    cases.push((
        "arr-assign".into(),
        "var a,b; [a,b]=[1,2]; a+'/'+b;".into(),
    ));
    cases.push((
        "obj-assign".into(),
        "var v; ({v}={v:4}); v;".into(),
    ));
    cases.push((
        "arr-assign-member".into(),
        "var o={}; [o.x]=[9]; o.x;".into(),
    ));
    cases.push((
        "str-iter".into(),
        "var [a,b,c] = 'xyz'; a+b+c;".into(),
    ));
    cases.push((
        "map-iter".into(),
        "var m=new Map([[1,'a'],[2,'b']]); var [k1,v1]=m; k1+v1;".into(),
    ));
    cases.push((
        "set-iter".into(),
        "var s=new Set([7]); var [z]=s; z;".into(),
    ));
    cases.push((
        "typed-array".into(),
        "var b=new ArrayBuffer(4); new Uint8Array(b).set([1,2,3,4]); var [p,q]=new Uint8Array(b); p+'/'+q;".into(),
    ));
    cases.push((
        "custom-iter".into(),
        "var o={}; o[Symbol.iterator]=function(){var i=0; return {next:function(){return i<3?{value:i++,done:false}:{value:undefined,done:true};}};}; var [m1,m2,m3,m4]=o; [m1,m2,m3,m4].join(',');".into(),
    ));
    cases.push(("non-iterable".into(), "var [a] = 5; a;".into()));
    cases.push(("undef-src".into(), "var [a] = undefined; a;".into()));
    cases.push(("null-src".into(), "var [a] = null; a;".into()));
    cases.push(("dup-binding".into(), "var [q,q] = [1,2]; q;".into()));
    cases.push((
        "for-of-decl".into(),
        "var acc=[]; for (const [k,v] of new Map([[1,2]])) acc.push(k+v); acc.join('');".into(),
    ));
    cases.push((
        "for-of-array".into(),
        "var acc=[]; for (const [a] of [[1],[2]]) acc.push(a); acc.join('');".into(),
    ));
    cases.push((
        "for-of-array-in".into(),
        "var k=[]; for (var x in {a:1,b:2}) k.push(x); k.join(',');".into(),
    ));
    cases.push((
        "hoist".into(),
        "var [g] = [1]; function f(){ return g; } f();".into(),
    ));
    cases.push((
        "arg-pattern".into(),
        "function f([a,b]){return a+b;} f([2,3]);".into(),
    ));
    cases.push((
        "short-source".into(),
        "var [a,b] = [1]; a+'/'+(b===undefined);".into(),
    ));
    cases.push((
        "multi-decl-mixed".into(),
        "var [a,{b}], c = 5; a+'/'+b+'/'+c;".into(),
    ));
    cases.push((
        "let-const".into(),
        "let [a] = [1]; const {b} = {b:2}; a+b;".into(),
    ));
    cases.push((
        "assignment-value-eval-order".into(),
        "var log=[]; function mk(v){ log.push(v); return v; } var [a=mk('d')] = [undefined]; log.join('');".into(),
    ));
    cases.push((
        "destructure-primitive-string-obj".into(),
        "var {length} = 'abc'; length;".into(),
    ));
    cases.push((
        "rest-on-string".into(),
        "var [a,...r] = 'abc'; a+'/'+r.join('-');".into(),
    ));
    cases.push((
        "deep-default".into(),
        "var {a:{b = 4} = {}} = {}; b;".into(),
    ));

    for (label, source) in cases {
        let mut parsed = parse_document("<!doctype html><p>probe</p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let result = match CompiledScript::compile(&source, &RuntimeLimits::default()) {
            Ok(script) => match runtime.execute_compiled(&mut parsed.dom, &script) {
                Ok(outcome) => format!("OK {}", outcome.value.to_js_string()),
                Err(error) => format!("RUNTIME-ERR {error}"),
            },
            Err(error) => format!("COMPILE-ERR {error}"),
        };
        println!("{label:32} {result}");
    }
}