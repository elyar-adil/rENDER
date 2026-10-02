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
