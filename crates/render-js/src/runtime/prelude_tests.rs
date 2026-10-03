//! The self-hosted built-ins in `prelude.js`.

use crate::JsRuntime;
use render_html::parse_document;

fn eval(source: &str) -> Result<String, crate::JsError> {
    let mut dom = parse_document(
        "<!doctype html><html><head><title>t</title></head><body><div id=a><p id=b>x</p></div></body></html>",
    )
    .dom;
    let mut runtime = JsRuntime::new(&dom);
    let result = runtime
        .execute(&mut dom, source)
        .map(|outcome| outcome.value.to_js_string());
    assert!(
        runtime.prelude_error().is_none(),
        "prelude failed: {:?}",
        runtime.prelude_error()
    );
    result
}

fn ok(source: &str) -> String {
    eval(source).unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

#[test]
fn the_prelude_installs_without_error() {
    assert_eq!(ok("typeof Object.fromEntries"), "function");
}

#[test]
fn object_helpers() {
    assert_eq!(ok("Object.fromEntries([['a', 1], ['b', 2]]).b"), "2");
    assert_eq!(ok("Object.fromEntries(new Map([['k', 'v']])).k"), "v");
    assert_eq!(
        ok("[Object.is(NaN, NaN), Object.is(0, -0), Object.is(1, 1)].join()"),
        "true,false,true"
    );
    assert_eq!(
        ok(
            "var g = Object.groupBy([1, 2, 3, 4], function (n) { return n % 2 ? 'odd' : 'even'; }); g.odd.join() + '|' + g.even.join()"
        ),
        "1,3|2,4"
    );
    assert_eq!(ok("Object.keys(Object).indexOf('fromEntries')"), "-1");
}

#[test]
fn array_helpers() {
    assert_eq!(ok("Array.of(7, 8).join()"), "7,8");
    assert_eq!(ok("[1, 2, 3, 4].fill(0, 1, 3).join()"), "1,0,0,4");
    assert_eq!(ok("[1, 2, 3].fill(9, -1).join()"), "1,2,9");
    assert_eq!(
        ok("[1, 2].flatMap(function (n) { return [n, n * 10]; }).join()"),
        "1,10,2,20"
    );
    assert_eq!(
        ok(
            "[1, 2, 1].lastIndexOf(1) + ',' + [1, 2, 1].lastIndexOf(1, -2) + ',' + [1].lastIndexOf(5)"
        ),
        "2,0,-1"
    );
    assert_eq!(ok("[1, 2, 3, 4, 5].copyWithin(0, 3).join()"), "4,5,3,4,5");
    assert_eq!(
        ok("var a = [3, 1, 2]; a.toSorted().join() + '|' + a.join()"),
        "1,2,3|3,1,2"
    );
    assert_eq!(
        ok("var a = [1, 2, 3]; a.toReversed().join() + '|' + a.join()"),
        "3,2,1|1,2,3"
    );
    assert_eq!(ok("[1, 2, 3].toSpliced(1, 1, 'x', 'y').join()"), "1,x,y,3");
    assert_eq!(ok("[1, 2, 3].with(-1, 9).join()"), "1,2,9");
    assert_eq!(
        eval("[1].with(5, 0)").unwrap_err().kind(),
        crate::JsErrorKind::Throw
    );
}

#[test]
fn string_helpers() {
    assert_eq!(
        ok(
            "Array.from('a1b22'.matchAll(/\\d+/g)).map(function (m) { return m[0] + '@' + m.index; }).join()"
        ),
        "1@1,22@3"
    );
    assert_eq!(
        ok("var n = 0; for (var m of 'aaa'.matchAll('a')) n++; n"),
        "3"
    );
    assert_eq!(
        ok("' x '.trimLeft() + '|' + ' x '.trimRight() + '|'"),
        "x | x|"
    );
}

#[test]
fn promise_and_error_helpers() {
    assert_eq!(
        ok(
            "var r = Promise.withResolvers(); r.resolve(1); (r.promise instanceof Promise) + ',' + typeof r.reject"
        ),
        "true,function"
    );
    assert_eq!(
        ok("var o = {}; Error.captureStackTrace(o); typeof o.stack"),
        "string"
    );
    assert_eq!(
        ok("Number.parseFloat === parseFloat && Number.parseInt === parseInt"),
        "true"
    );
}

#[test]
fn weak_references() {
    assert_eq!(ok("var o = {}; new WeakRef(o).deref() === o"), "true");
    assert_eq!(
        ok("var r = new FinalizationRegistry(function () {}); r.register({}, 1); r.unregister({})"),
        "false"
    );
}

#[test]
fn immediate_and_idle_callbacks_exist() {
    assert_eq!(
        ok(
            "typeof setImmediate + typeof clearImmediate + typeof requestIdleCallback + typeof cancelIdleCallback"
        ),
        "functionfunctionfunctionfunction"
    );
}

const CUSTOM: &str = "var log = [];
class XA extends HTMLElement {
  static get observedAttributes() { return ['k']; }
  constructor() { super(); log.push('ctor'); }
  connectedCallback() { log.push('conn'); }
  disconnectedCallback() { log.push('disc'); }
  attributeChangedCallback(n, o, v) { log.push('attr:' + n + ':' + o + ':' + v); }
}";

fn custom(body: &str) -> String {
    ok(&format!("{CUSTOM}\n{body}\nlog.join()"))
}

#[test]
fn custom_element_registry_and_validation() {
    assert_eq!(ok("typeof customElements.define"), "function");
    assert_eq!(
        ok(
            "class A extends HTMLElement {}; customElements.define('x-a', A); [customElements.get('x-a') === A, customElements.getName(A), customElements.get('x-b')].join()"
        ),
        "true,x-a,"
    );
    assert_eq!(
        ok(
            "class A extends HTMLElement {}; try { customElements.define('nodash', A) } catch (e) { e.name }"
        ),
        "SyntaxError"
    );
    assert_eq!(
        ok(
            "class A extends HTMLElement {}; class B extends HTMLElement {}; customElements.define('x-a', A); try { customElements.define('x-a', B) } catch (e) { e.name }"
        ),
        "NotSupportedError"
    );
}

#[test]
fn custom_element_constructor_and_create_element() {
    assert_eq!(
        custom(
            "customElements.define('x-a', XA); var e = document.createElement('x-a'); log.push(e instanceof XA, e instanceof HTMLElement, e.localName); var n = new XA(); log.push(n.localName, n.isConnected)"
        ),
        "ctor,true,true,x-a,ctor,x-a,false"
    );
    assert_eq!(
        ok("try { new HTMLElement() } catch (e) { e.name }"),
        "TypeError"
    );
}

#[test]
fn custom_element_upgrade_on_define_and_connection() {
    assert_eq!(
        custom(
            "var e = document.createElement('x-a'); e.setAttribute('k', 'v'); document.body.appendChild(e); customElements.define('x-a', XA); log.push(e instanceof XA)"
        ),
        "ctor,attr:k:null:v,conn,true"
    );
    assert_eq!(
        custom(
            "customElements.define('x-a', XA); document.getElementById('a').innerHTML = '<x-a></x-a>'"
        ),
        "ctor,conn"
    );
}

#[test]
fn custom_element_lifecycle_callbacks() {
    assert_eq!(
        custom(
            "customElements.define('x-a', XA); var e = document.createElement('x-a'); log.length = 0; var host = document.getElementById('a'); host.appendChild(e); e.setAttribute('k', '1'); e.setAttribute('other', '1'); e.setAttribute('k', '2'); e.removeAttribute('k'); host.removeChild(e)"
        ),
        "conn,attr:k:null:1,attr:k:1:2,attr:k:2:null,disc"
    );
    assert_eq!(
        custom(
            "customElements.define('x-a', XA); var host = document.getElementById('a'); host.innerHTML = '<p><x-a></x-a></p>'; host.innerHTML = ''"
        ),
        "ctor,conn,disc"
    );
    assert_eq!(
        custom(
            "customElements.define('x-a', XA); var e = document.createElement('x-a'); log.length = 0; document.body.appendChild(e); e.remove()"
        ),
        "conn,disc"
    );
}

#[test]
fn custom_element_when_defined_resolves() {
    assert_eq!(
        ok(
            "var r = 'no'; customElements.whenDefined('x-w').then(function (c) { r = typeof c; }); class W extends HTMLElement {}; customElements.define('x-w', W); r"
        ),
        "no"
    );
}

#[test]
fn super_passes_the_derived_new_target_to_a_plain_function_parent() {
    assert_eq!(
        ok(
            "var seen; function B() { seen = new.target; } class D extends B {} class E extends D {} new E(); seen === E"
        ),
        "true"
    );
    assert_eq!(
        ok("var seen; function B() { seen = new.target; } new B(); seen === B"),
        "true"
    );
}

#[test]
fn custom_element_reflecting_property_writes_react() {
    assert_eq!(
        custom(
            "customElements.define('x-a', XA); var e = document.createElement('x-a'); log.length = 0; e.id = 'z'; document.body.appendChild(e); e.textContent = 'hi'"
        ),
        "conn"
    );
    assert_eq!(
        ok(
            "var log = []; class Y extends HTMLElement { static get observedAttributes() { return ['id', 'class']; } attributeChangedCallback(n, o, v) { log.push(n + ':' + o + ':' + v); } } customElements.define('x-y', Y); var e = document.createElement('x-y'); e.id = 'z'; e.className = 'c'; log.join()"
        ),
        "id:null:z,class:null:c"
    );
}

#[test]
fn abort_signal_interface_and_factories() {
    assert_eq!(
        ok(
            "var c = new AbortController(); [c.signal instanceof AbortSignal, c.signal instanceof EventTarget, typeof AbortSignal.abort, c.signal.aborted].join()"
        ),
        "true,true,function,false"
    );
    assert_eq!(
        ok(
            "var s = AbortSignal.abort('why'); var r; try { s.throwIfAborted() } catch (e) { r = e } [s.aborted, s.reason, r].join()"
        ),
        "true,why,why"
    );
    assert_eq!(
        ok(
            "var a = new AbortController(); var s = AbortSignal.any([a.signal]); var hit = 0; s.addEventListener('abort', function () { hit++ }); a.abort('x'); [s.aborted, s.reason, hit].join()"
        ),
        "true,x,1"
    );
    assert_eq!(
        ok("try { new AbortSignal() } catch (e) { e.name }"),
        "TypeError"
    );
}

#[test]
fn headers_behave_like_the_fetch_standard() {
    assert_eq!(
        ok(
            "var h = new Headers({'Content-Type': 'a'}); h.append('x-k', '1'); h.append('X-K', ' 2 '); [h.get('content-type'), h.get('x-k'), h.has('nope'), h.get('nope'), Array.from(h).join('|')].join()"
        ),
        "a,1, 2,false,,content-type,a|x-k,1, 2"
    );
    assert_eq!(
        ok(
            "var h = new Headers([['b', '1'], ['a', '2']]); h.delete('b'); h.set('c', 'x'); Array.from(h.keys()).join() + Array.from(new Headers(h).values()).join()"
        ),
        "a,c2,x"
    );
    assert_eq!(
        ok("try { new Headers({'bad name': 'x'}) } catch (e) { e.name }"),
        "TypeError"
    );
}

#[test]
fn request_carries_method_headers_and_body() {
    assert_eq!(
        ok(
            "var r = new Request('/p', { method: 'post', headers: { A: 'b' }, body: 'hi' }); [r.method, r.headers.get('a'), r.url.indexOf('/p') >= 0, r.clone().method].join()"
        ),
        "POST,b,true,POST"
    );
    assert_eq!(
        ok("try { new Request('/p', { body: 'x' }) } catch (e) { e.name }"),
        "TypeError"
    );
    assert_eq!(
        ok(
            "var out; new Request('/p', { method: 'POST', body: '{\"a\":1}' }).json().then(function (v) { out = v.a }); out"
        ),
        "undefined"
    );
}

#[test]
fn array_join_stringifies_object_elements() {
    assert_eq!(ok("String([['a', 1], [2, [3]]])"), "a,1,2,3");
    assert_eq!(
        ok("[{ toString: function () { return 'o'; } }, null, 1].join('-')"),
        "o--1"
    );
    assert_eq!(ok("var a = [1]; a.push(a); a.join()"), "1,");
}

#[test]
fn crypto_random_values_and_uuid() {
    assert_eq!(
        ok(
            "var a = new Uint32Array(8); var r = crypto.getRandomValues(a); var b = crypto.getRandomValues(new Int16Array(16)); var c = crypto.getRandomValues(new Uint8Array(32)); [r === a, Array.from(a).some(function (v) { return v > 65535 }), Array.from(b).some(function (v) { return v < 0 }), Array.from(c).some(function (v) { return v > 0 })].join()"
        ),
        "true,true,true,true"
    );
    assert_eq!(
        ok(
            "/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(crypto.randomUUID()) && crypto.randomUUID() !== crypto.randomUUID()"
        ),
        "true"
    );
    assert_eq!(
        ok("try { crypto.getRandomValues(new Float32Array(1)) } catch (e) { e.name }"),
        "TypeMismatchError"
    );
    assert_eq!(
        ok("try { crypto.getRandomValues(new Uint8Array(65537)) } catch (e) { e.name }"),
        "QuotaExceededError"
    );
    assert_eq!(
        ok("typeof __render_random_bytes + typeof crypto.subtle"),
        "undefinedundefined"
    );
}

#[test]
fn typed_array_prototype_methods() {
    assert_eq!(
        ok(
            "var t = new Uint8Array([3, 1, 2]); [t.some(function (v) { return v > 2 }), t.every(function (v) { return v > 0 }), t.find(function (v) { return v < 3 }), t.findIndex(function (v) { return v === 2 }), t.findLast(function (v) { return v < 3 }), t.at(-1), t.reduce(function (a, v) { return a + v }), t.reduceRight(function (a, v) { return a + '' + v }), t.lastIndexOf(1)].join()"
        ),
        "true,true,1,2,2,2,6,213,1"
    );
    assert_eq!(
        ok(
            "var t = new Int16Array([10, 9, 1, -5]); var s = t.toSorted(); [t.join(), s.join(), s instanceof Int16Array, t.toReversed().join(), t.with(0, 7).join(), Array.from(t.keys()).join(), String(Array.from(t.entries())[1])].join('|')"
        ),
        "10,9,1,-5|-5,1,9,10|true|-5,1,9,10|7,9,1,-5|0,1,2,3|1,9"
    );
    assert_eq!(
        ok("var t = new Uint8Array([1, 2, 3, 4, 5]); t.copyWithin(0, 3); t.reverse(); t.join()"),
        "5,4,3,5,4"
    );
}
