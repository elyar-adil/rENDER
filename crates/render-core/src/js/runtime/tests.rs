use std::collections::BTreeMap;

use crate::html::parse_document;
use crate::js::JsValue;
use crate::js::{ElementRect, FetchOutcome, JsRuntime};
use url::Url;

/// Run every queued microtask (including ones queued by earlier microtasks)
/// until the runtime has none left.
fn drain_microtasks(runtime: &mut JsRuntime, dom: &mut crate::dom::Dom) {
    loop {
        let pending = runtime.take_pending_microtasks();
        if pending.is_empty() {
            return;
        }
        for microtask in pending {
            runtime
                .invoke_microtask(dom, microtask)
                .expect("microtask executes");
        }
    }
}

#[test]
fn location_exposes_normalized_committed_url_components() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let url = Url::parse("https://user:pass@example.test:8443/a/b?q=rust#part").expect("test URL");
    let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r##"
                    window === self && self === globalThis && globalThis === window &&
                    window.location === location && document.location === location &&
                    location.href === "https://user:pass@example.test:8443/a/b?q=rust#part" &&
                    location.origin === "https://example.test:8443" &&
                    location.protocol === "https:" && location.host === "example.test:8443" &&
                    location.hostname === "example.test" && location.port === "8443" &&
                    location.pathname === "/a/b" && location.search === "?q=rust" &&
                    location.hash === "#part" && location.toString() === location.href;
                "##,
        )
        .expect("Location reads should execute");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn array_some_short_circuits_on_the_first_matching_callback() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
            .execute(
                &mut parsed.dom,
                "var calls = 0; var matched = [1, 2, 3].some(function(value) { calls += 1; return value === 2; }); matched && calls === 2;",
            )
            .expect("Array.prototype.some should execute");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn location_href_writes_and_assign_replace_queue_navigations() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let url = Url::parse("https://example.test/current").expect("test URL");
    let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                    location.href = "/next";
                    location.assign("https://other.test/a");
                    location.replace("/swap");
                "#,
        )
        .expect("Location navigation writes should execute");
    assert_eq!(
        runtime.take_pending_navigations(),
        vec![
            crate::js::NavigationRequest {
                url: "https://example.test/next".to_owned(),
                replace: false
            },
            crate::js::NavigationRequest {
                url: "https://other.test/a".to_owned(),
                replace: false
            },
            crate::js::NavigationRequest {
                url: "https://example.test/swap".to_owned(),
                replace: true
            },
        ]
    );
    assert!(runtime.take_pending_navigations().is_empty());
}

#[test]
fn location_non_href_writes_still_fail_instead_of_faking_navigation() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let url = Url::parse("https://example.test/current").expect("test URL");
    let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
    let error = runtime
        .execute(&mut parsed.dom, "location.pathname = '/next';")
        .expect_err("unsupported navigation must be explicit");
    assert_eq!(error.kind(), crate::js::JsErrorKind::Type);
    assert!(error.message().contains("embedding browser"));
}

#[test]
fn navigator_exposes_browser_bootstrap_properties() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"navigator.appName === "Netscape" &&
                    navigator.userAgent === "Mozilla/5.0 rENDER/0.1" &&
                    navigator.language === "zh-CN" &&
                    navigator.cookieEnabled && navigator.onLine;"#,
        )
        .expect("navigator feature detection should execute");

    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn event_target_registers_removes_and_cancels_listeners() {
    let mut parsed = parse_document("<!doctype html><button id='button'>go</button>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var button = document.getElementById("button");
                    var calls = 0;
                    function listener(event) {
                        calls = calls + 1;
                        event.preventDefault();
                    }
                    button.addEventListener("activate", listener);
                    button.addEventListener("activate", listener);
                    var event = new Event("activate", { cancelable: true });
                    var accepted = button.dispatchEvent(event);
                    button.removeEventListener("activate", listener);
                    button.dispatchEvent(new Event("activate"));
                    calls + ":" + accepted + ":" + event.defaultPrevented;
                "#,
        )
        .expect("EventTarget methods should dispatch a cancelable event");
    assert_eq!(outcome.value, JsValue::String("1:false:true".to_owned()));
}

#[test]
fn bubbling_event_exposes_target_current_target_and_listener_this() {
    let mut parsed =
        parse_document("<!doctype html><div id='parent'><button id='child'>go</button></div>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var parent = document.getElementById("parent");
                    var child = document.getElementById("child");
                    var observed = false;
                    function listener(event) {
                        observed = event.target === child &&
                            event.currentTarget === parent && this === parent;
                    }
                    parent.addEventListener("activate", listener);
                    var event = new Event("activate", { bubbles: true });
                    child.dispatchEvent(event);
                    observed && event.currentTarget === null;
                "#,
        )
        .expect("bubbling should traverse DOM ancestors");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn object_reflection_and_assignment_cover_common_runtime_usage() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var source = { b: 2, a: 1 };
                    var target = Object.assign({}, source);
                    var descriptor = Object.getOwnPropertyDescriptor(target, "a");
                    Object.defineProperty(target, "hidden", { value: 9, enumerable: false, writable: false, configurable: false });
                    Object.keys(target)[0] + Object.keys(target)[1] + Object.values(target)[0] + Object.values(target)[1] + Object.hasOwn(target, "hidden") + descriptor.value;
                "#,
            )
            .expect("Object builtins should execute");
    assert_eq!(outcome.value, JsValue::String("ba21true1".to_owned()));
}

#[test]
fn object_create_and_constructor_preserve_prototypes() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
            .execute(
                &mut parsed.dom,
                r"
                    var prototype = { answer: 42 };
                    var child = Object.create(prototype);
                    var wrapped = { value: 7 };
                    Object.getPrototypeOf(child) === prototype && child.answer === 42 && Object(wrapped) === wrapped;
                ",
            )
            .expect("Object.create and Object() should execute");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn object_prototype_methods_cover_ordinary_arrays_and_null_prototypes() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var ordinary = { visible: 1 };
                    var dictionary = Object.create(null);
                    Object.getPrototypeOf(ordinary) === Object.prototype &&
                        ordinary.hasOwnProperty("visible") &&
                        !ordinary.hasOwnProperty("toString") &&
                        ordinary.propertyIsEnumerable("visible") &&
                        !ordinary.propertyIsEnumerable("hasOwnProperty") &&
                        Object.prototype.isPrototypeOf(ordinary) &&
                        Object.prototype.isPrototypeOf([]) &&
                        Array.hasOwnProperty("prototype") &&
                        dictionary.hasOwnProperty === undefined;
                "#,
        )
        .expect("Object.prototype methods should execute");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn array_constructor_and_length_follow_common_ecmascript_semantics() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var empty = new Array();
                    var sized = Array(3);
                    var one = Array("x");
                    var many = new Array(1, 2, 3);
                    sized[4] = 5;
                    sized.length = 2;
                    var unshifted = many.unshift(0);
                    empty.length + ":" + sized.length + ":" +
                        (sized[4] === undefined) + ":" + one[0] + ":" +
                        many.join(",") + ":" + unshifted + ":" + Array.isArray(many);
                "#,
        )
        .expect("Array constructor and length semantics should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("0:2:true:x:0,1,2,3:4:true".to_owned())
    );
}

#[test]
fn primitive_prototypes_and_native_function_call_are_wired() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    [
                        8e-5.toFixed(3),
                        1..toPrecision(void 0),
                        (1.2).toPrecision(3),
                        Number.prototype.constructor === Number,
                        Boolean.prototype.constructor === Boolean,
                        Array.prototype.slice.call({0: "ok", length: 1}, 0)[0],
                        (function(floor) { return floor(1.9); })(Math.floor)
                    ].join("|");
                "#,
        )
        .expect("primitive methods and native call inheritance should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("0.000|1|1.20|true|true|ok|1".to_owned())
    );
}

#[test]
fn dom_interfaces_and_keyed_collections_cover_site_bootstrap_usage() {
    let mut parsed = parse_document("<!doctype html><main id='app'></main>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var key = {};
                    var map = new Map([[key, 1], ["x", 2]]);
                    map.set(NaN, 3);
                    var total = 0;
                    map.forEach(function(value) { total += value; });
                    var first = map.entries().next();
                    var set = new Set([1, 2, 2]);
                    var weak = new WeakMap([[key, "ok"]]);
                    var element = document.createElement("section");
                    [
                        typeof Element,
                        element instanceof Element,
                        Element.prototype.matches === element.matches,
                        map.size, map.get(key), map.has(NaN), total,
                        first.done, first.value[1], set.size, weak.get(key)
                    ].join("|");
                "#,
        )
        .expect("DOM constructors and keyed collections should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("function|true|true|3|1|true|6|false|1|2|ok".to_owned())
    );
}

#[test]
fn template_literals_and_surrogate_pairs_execute() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var site = "QQ";
                    `${site}-${1 + 2}-\uD83D\uDE00`;
                "#,
        )
        .expect("template interpolation should execute");
    assert_eq!(outcome.value, JsValue::String("QQ-3-😀".to_owned()));
}

#[test]
fn array_length_reads_use_to_length_for_generic_array_methods() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var object = { 0: "a", length: "1.9" };
                    var joined = Array.prototype.join.call(object, "-");
                    var reduced = Array.prototype.reduce.call("es5", function(value, item, index, source) {
                        return source;
                    });
                    [joined, typeof reduced, reduced[0], reduced.length].join("|");
                "#,
            )
            .expect("generic array methods should normalize length");
    assert_eq!(outcome.value, JsValue::String("a|object|e|3".to_owned()));
}

#[test]
fn for_in_enumerates_prototypes_and_delete_honors_descriptors() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var prototype = { inherited: 1 };
                    var object = Object.create(prototype);
                    object.own = 2;
                    Object.defineProperty(object, "hidden", { value: 3, enumerable: false });
                    Object.defineProperty(object, "fixed", { value: 4, enumerable: true, configurable: false });
                    var names = "";
                    for (var name in object) { names = names + name + ","; }
                    var removed = delete object.own;
                    var retained = delete object.fixed;
                    names + removed + "," + retained + "," + object.own + "," + object.fixed;
                "#,
            )
            .expect("for-in and delete should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("fixed,own,inherited,true,false,undefined,4".to_owned())
    );
}

#[test]
fn function_call_bind_and_typeof_share_callable_semantics() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    function add(left, right) { return this.base + left + right; }
                    var bound = add.bind({ base: 10 }, 2);
                    typeof Function === "function" &&
                        typeof Function.prototype === "function" &&
                        typeof bound === "function" &&
                        add.call({ base: 1 }, 3, 4) === 8 &&
                        bound(5) === 17;
                "#,
        )
        .expect("Function.prototype call and bind should execute");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn property_helper_primordials_can_be_uncurried() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var join = Function.prototype.call.bind(Array.prototype.join);
                    var push = Function.prototype.call.bind(Array.prototype.push);
                    var values = ["a"];
                    push(values, "b", "c");
                    var target = { visible: 1 };
                    Object.defineProperty(target, "hidden", { value: 2, enumerable: false });
                    join(values, ";") + ":" +
                        join(Object.getOwnPropertyNames(target), ",") + ":" +
                        Math.pow(2, 5);
                "#,
        )
        .expect("propertyHelper primordial operations should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("a;b;c:visible,hidden:32".to_owned())
    );
}

#[test]
fn user_functions_expose_arguments_and_string_conversion() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    function inspect(value) {
                        var first = () => arguments[0];
                        return arguments.length + ":" + String(first()) + ":" + typeof String;
                    }
                    inspect(42, "extra");
                "#,
        )
        .expect("ordinary functions should expose arguments");
    assert_eq!(outcome.value, JsValue::String("2:42:function".to_owned()));
}

#[test]
fn native_error_constructors_share_the_error_prototype_contract() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var error = new TypeError("bad input");
                    var called = RangeError("out of range");
                    typeof Error + ":" +
                        (error instanceof TypeError) + ":" +
                        (error instanceof Error) + ":" +
                        (called instanceof RangeError) + ":" +
                        error.name + ":" + error.message + ":" + error.toString();
                "#,
        )
        .expect("native Error constructors should call and construct");

    assert_eq!(
        outcome.value,
        JsValue::String(
            "function:true:true:true:TypeError:bad input:TypeError: bad input".to_owned()
        )
    );
}

#[test]
fn native_errors_have_distinct_prototypes_and_optional_messages() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var empty = new Error();
                    var syntax = new SyntaxError("parse");
                    Object.getPrototypeOf(empty) === Error.prototype &&
                        Object.getPrototypeOf(syntax) === SyntaxError.prototype &&
                        Object.getPrototypeOf(SyntaxError.prototype) === Error.prototype &&
                        !empty.hasOwnProperty("message") &&
                        syntax.propertyIsEnumerable("message") === false &&
                        empty.toString() === "Error";
                "#,
        )
        .expect("native Error prototypes and descriptors should execute");

    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn bitwise_operators_follow_ecmascript_precedence_and_int32_semantics() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var flags = 6;
                    var jqueryToggle = flags ^ 1;
                    var precedence = 1 | 2 ^ 3 & 1;
                    var shifts = (1 << 31) + ":" + (-1 >> 1) + ":" + (-1 >>> 0);
                    var coercion = (NaN | 0) + ":" + (Infinity & 7) + ":" + (~0);
                    jqueryToggle + ":" + precedence + ":" + shifts + ":" + coercion;
                "#,
        )
        .expect("site-style bitwise expressions should execute");

    assert_eq!(
        outcome.value,
        JsValue::String("7:3:-2147483648:-1:4294967295:0:0:-1".to_owned())
    );
}

#[test]
fn bitwise_compound_assignment_evaluates_member_reference_once() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var calls = 0;
                    var values = [6];
                    function index() { calls += 1; return 0; }
                    values[index()] ^= 3;
                    values[index()] <<= 2;
                    values[index()] >>>= 1;
                    values[0] + ":" + calls;
                "#,
        )
        .expect("compound bitwise assignment should preserve a single reference evaluation");

    assert_eq!(outcome.value, JsValue::String("10:3".to_owned()));
}

#[test]
fn set_timeout_registers_timer_and_requests_scheduling() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(&mut parsed.dom, "setTimeout(function () {}, 250);")
        .expect("setTimeout should execute");
    let id = match outcome.value {
        JsValue::Number(id) => id,
        other => panic!("setTimeout should return a numeric id, got {other:?}"),
    };
    assert!((id - 1.0).abs() < f64::EPSILON, "unexpected timer id {id}");
    let requests = runtime.take_pending_timer_requests();
    assert_eq!(
        requests,
        vec![crate::js::TimerRequest::Schedule {
            id: 1,
            delay_ms: 250.0
        }]
    );
}

#[test]
fn fire_timer_invokes_callback_once_and_clear_removes_it() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r"
                    var hits = 0;
                    setTimeout(function () { hits += 1; }, 0);
                    var interval = setInterval(function () { hits += 10; }, 5);
                    clearInterval(interval);
                ",
        )
        .expect("timer registration should succeed");
    let _ = runtime.take_pending_timer_requests();

    // The timeout fires once; the cancelled interval stays silent.
    let rearm = runtime
        .fire_timer(&mut parsed.dom, 1)
        .expect("firing a registered timer succeeds");
    assert_eq!(rearm, None);
    assert!(runtime.take_pending_timer_requests().is_empty());

    runtime
        .execute(&mut parsed.dom, "hits")
        .map(|outcome| assert_eq!(outcome.value, JsValue::Number(1.0)))
        .expect("reading hits should work");
    assert!(runtime.fire_timer(&mut parsed.dom, 99).is_ok());
    assert_eq!(
        crate::js::ConsoleLevel::Log.label(),
        "log",
        "sanity check the level labels stay importable"
    );
}

#[test]
fn console_methods_buffer_messages_for_the_embedding() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                    console.log("hello", 42);
                    console.warn("careful");
                    console.error("boom");
                    console.info("info");
                    console.debug("debug");
                "#,
        )
        .expect("console calls should execute");
    let messages = runtime.take_console_messages();
    let rendered: Vec<_> = messages
        .iter()
        .map(|message| (message.level.label(), message.text.as_str()))
        .collect();
    assert_eq!(
        rendered,
        vec![
            ("log", "hello 42"),
            ("warn", "careful"),
            ("error", "boom"),
            ("info", "info"),
            ("debug", "debug"),
        ]
    );
    assert!(runtime.take_console_messages().is_empty());
}

#[test]
fn request_animation_frame_registers_zero_delay_one_shot() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(&mut parsed.dom, r"requestAnimationFrame(function () {});")
        .expect("rAF registration should succeed");
    let requests = runtime.take_pending_timer_requests();
    assert!(matches!(
        requests.as_slice(),
        [crate::js::TimerRequest::Schedule { delay_ms: 0.0, .. }]
    ));
    let rearm = runtime
        .fire_timer(&mut parsed.dom, 1)
        .expect("animation frame callback fires");
    assert_eq!(rearm, None);
}

#[test]
fn inner_html_round_trips_and_setter_replaces_children() {
    let mut parsed = parse_document("<!doctype html><div id='host'><p>old</p></div>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var host = document.getElementById("host");
                    var before = host.innerHTML;
                    host.innerHTML = "<b>new</b> text &amp; more<!-- c -->";
                    var after = host.innerHTML;
                    before + "|" + after + "|" + host.children.length;
                "#,
        )
        .expect("innerHTML accessors should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("<p>old</p>|<b>new</b> text &amp; more<!-- c -->|1".to_owned()),
        "children counts only the element child; text and comment are preserved"
    );
}

#[test]
fn inner_html_import_respects_node_creation_limits() {
    let mut parsed = parse_document("<!doctype html><div id='host'></div>");
    let limits = crate::js::RuntimeLimits {
        max_dom_nodes_created: 2,
        ..crate::js::RuntimeLimits::default()
    };
    let mut runtime = JsRuntime::with_limits(&parsed.dom, limits);
    let error = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var host = document.getElementById("host");
                    host.innerHTML = "<i>a</i><i>b</i><i>c</i>";
                "#,
        )
        .expect_err("importing past the node budget must fail");
    assert_eq!(error.kind(), crate::js::JsErrorKind::ResourceLimit);
}

#[test]
fn style_declaration_reads_writes_and_clears_inline_declarations() {
    let mut parsed = parse_document("<!doctype html><p id='target'>x</p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var p = document.getElementById("target");
                    p.style.color = "red";
                    p.style.setProperty("background-color", "blue", "important");
                    var color = p.style.color;
                    var viaGet = p.style.getPropertyValue("color");
                    var length = p.style.length;
                    var first = p.style.item(0);
                    var cssText = p.style.cssText;
                    p.style.removeProperty("color");
                    var afterRemove = p.style.getPropertyValue("color");
                    color + ":" + viaGet + ":" + length + ":" + first + ":" +
                        cssText + ":" + afterRemove + ":" +
                        p.getAttribute("style");
                "#,
        )
        .expect("CSSStyleDeclaration operations should execute");
    // Declaration order follows source order; removing `color` leaves only
    // the important background declaration on the element.
    assert_eq!(
        outcome.value,
        JsValue::String(
            "red:red:2:color:color: red; background-color: blue !important;::\
                 background-color: blue !important;"
                .to_owned()
        ),
        "unexpected inline style state"
    );
}

#[test]
fn get_rect_bounds_returns_zeroes_without_layout() {
    let mut parsed = parse_document("<!doctype html><p id='p'>x</p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var rect = document.getElementById("p").getBoundingClientRect();
                    rect.width === 0 && rect.height === 0 && rect.top === 0 &&
                        rect.left === 0 && rect.right === 0 && rect.bottom === 0;
                "#,
        )
        .expect("getBoundingClientRect without geometry should return zeros");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn get_rect_bounds_reports_installed_geometry() {
    let mut parsed = parse_document("<!doctype html><p id='p'>x</p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let mut geometry = std::collections::BTreeMap::new();
    let body =
        crate::js::runtime::builtins::dom::find_body_node(&parsed.dom, parsed.dom.document())
            .expect("body exists");
    let paragraph = parsed.dom.children(body).unwrap_or_default()[0];
    geometry.insert(
        paragraph.as_u64(),
        crate::js::ElementRect {
            x: 8.0,
            y: 16.0,
            width: 100.0,
            height: 20.0,
        },
    );
    runtime.install_element_geometry(geometry);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var rect = document.getElementById("p").getBoundingClientRect();
                    rect.x + "," + rect.y + "," + rect.width + "," + rect.height + "," +
                        rect.right + "," + rect.bottom;
                "#,
        )
        .expect("geometry-backed rect should read back");
    assert_eq!(
        outcome.value,
        JsValue::String("8,16,100,20,108,36".to_owned())
    );
}

#[test]
fn dispatch_dom_event_walks_ancestors_and_reports_prevention() {
    let mut parsed =
        parse_document("<!doctype html><div id='parent'><button id='child'>go</button></div>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                    document.getElementById("parent")
                        .addEventListener("custompress", function (event) {
                            event.preventDefault();
                        });
                "#,
        )
        .expect("listener registration should succeed");
    let child = {
        let body =
            crate::js::runtime::builtins::dom::find_body_node(&parsed.dom, parsed.dom.document())
                .expect("body exists");
        let div = parsed.dom.children(body).unwrap_or_default()[0];
        parsed.dom.children(div).unwrap_or_default()[0]
    };
    let allowed = runtime
        .dispatch_dom_event(&mut parsed.dom, child, "custompress", true, true, &[])
        .expect("trusted dispatch should run listeners");
    assert!(!allowed, "preventDefault must cancel the default action");
}

#[test]
fn regex_literals_support_exec_groups_and_flags() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var re = /(\w+)-(\d+)/i;
                    var first = re.exec("item-42 rest");
                    var flags_ok = re.global === false && re.ignoreCase === true &&
                        re.multiline === false && re.sticky === false;
                    [first[0], first[1], first[2], first.index, first.input, flags_ok].join("|");
                "#,
        )
        .expect("regex literal should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("item-42|item|42|0|item-42 rest|true".to_owned())
    );
}

#[test]
fn regex_global_test_and_sticky_track_last_index() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var global = /a/g;
                    var sticky = /a/y;
                    sticky.lastIndex = 2;
                    [
                        global.test("banana"), global.test("banana"),
                        global.test("banana"), global.test("banana"), global.test("banana"),
                        sticky.test("banana"), sticky.lastIndex
                    ].join(",");
                "#,
        )
        .expect("regex flag tests should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("true,true,true,false,true,false,0".to_owned())
    );
}

#[test]
fn string_methods_cover_indexing_slicing_and_case() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var text = "  Hello, World!  ";
                    [
                        text.trim().charAt(4), text.trim().charCodeAt(0),
                        text.indexOf("World") - 2, "abc".lastIndexOf("b"),
                        "hello".toUpperCase(), "WORLD".toLowerCase(),
                        "abcdef".slice(1, 3), "abcdef".slice(-3),
                        "abcdef".substring(4, 2), "abc".concat("def", "ghi")
                    ].join("|");
                "#,
        )
        .expect("string methods should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("o|72|7|1|HELLO|world|bc|def|cd|abcdefghi".to_owned())
    );
}

#[test]
fn string_replace_expands_groups_and_honors_global() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    [
                        "a-b-c".replace(/(\w)-(\w)/, "$2_$1"),
                        "a-b-c".replace(/-/g, "+"),
                        "2026-08-23".replace(/(\d{4})-(\d{2})-(\d{2})/, "$3/$2/$1")
                    ].join("|");
                "#,
        )
        .expect("string replace should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("b_a-c|a+b+c|23/08/2026".to_owned())
    );
}

#[test]
fn string_split_match_search_interoperate_with_regex() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var matched = "ab12cd34".match(/\d+/g);
                    [
                        "a,b,,c".split(",").length,
                        "one two  three".split(/\s+/).join("/"),
                        matched.length + ":" + matched.join("."),
                        "find the needle".search(/needle/),
                        "nope".search(/zzz/)
                    ].join("|");
                "#,
        )
        .expect("regex-aware string methods should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("4|one/two/three|2:12.34|9|-1".to_owned())
    );
}

#[test]
fn json_and_date_builtins_cover_real_world_bootstrap_scripts() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var value = JSON.parse('{"name":"rENDER","items":[1,true,null]}');
                    var encoded = JSON.stringify(value);
                    var date = new Date("2026-08-23T12:34:56.789Z");
                    [encoded, date.getTime(), date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate(),
                     date.getUTCHours(), date.toISOString(), Date.parse("2026-08-23T00:00:00Z")].join("|");
                "#,
            )
            .expect("JSON and Date builtins should execute");
    assert_eq!(
            outcome.value,
            JsValue::String(
                "{\"name\":\"rENDER\",\"items\":[1,true,null]}|1787488496789|2026|7|23|12|2026-08-23T12:34:56.789Z|1787443200000"
                    .to_owned()
            )
        );
}

#[test]
fn global_this_is_available_to_feature_detection() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var root = globalThis || self || window;
                    [typeof globalThis, root === window, "URLSearchParams" in root].join("|");
                "#,
        )
        .expect("globalThis feature detection should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("object|true|true".to_owned())
    );
}

#[test]
fn typeof_missing_bindings_short_circuits_optional_globals() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"[typeof define, ("function" == typeof define && define.amd) ? "yes" : "no"].join("|")"#,
            )
            .expect("optional global feature detection should execute");
    assert_eq!(outcome.value, JsValue::String("undefined|no".to_owned()));
}

#[test]
fn modern_binding_spread_computed_and_string_builtins_execute() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    const key = "side";
                    const source = {head: 1, side: 7, tail: 9};
                    const {head: a, ...rest} = source;
                    const [[b], c] = [[2], 3];
                    let x, y; [x, y] = [4, 5];
                    const object = {[key]: 6, ...rest};
                    [String.fromCharCode(65, 66), String.fromCodePoint(128512),
                     a, b, c, x, y, object.side, object.tail, 2 ** 3].join("|");
                    let pairs = "";
                    for (const [name, value] of Object.entries(object)) {
                        pairs += name + value;
                    }
                    [String.fromCharCode(65, 66), String.fromCodePoint(128512),
                     a, b, c, x, y, object.side, object.tail, 2 ** 3, pairs].join("|");
                "#,
        )
        .expect("modern syntax and string constructors should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("AB|😀|1|2|3|4|5|7|9|8|side7tail9".to_owned())
    );
}

#[test]
fn intersection_observer_reports_viewport_entries_and_drives_src_mutation() {
    let mut parsed = parse_document("<!doctype html><img id=lazy data-src=loaded.png>");
    let image = {
        let body =
            crate::js::runtime::builtins::dom::find_body_node(&parsed.dom, parsed.dom.document())
                .unwrap();
        parsed.dom.children(body).unwrap_or_default()[0]
    };
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime.install_viewport(800.0, 600.0, 0.0, 0.0);
    let mut geometry = BTreeMap::new();
    geometry.insert(
        image.as_u64(),
        ElementRect {
            x: 20.0,
            y: 30.0,
            width: 200.0,
            height: 100.0,
        },
    );
    runtime.install_element_geometry(geometry);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var observed = "";
                    var target = document.getElementById("lazy");
                    var observer = new IntersectionObserver(function(entries) {
                        observed = entries[0].isIntersecting + ":" +
                            entries[0].intersectionRatio + ":" +
                            entries[0].boundingClientRect.top;
                        if (entries[0].isIntersecting) target.src = target.getAttribute("data-src");
                    });
                    observer.observe(target);
                "#,
        )
        .expect("observer registration should execute");
    let tasks = runtime.take_pending_microtasks();
    assert_eq!(tasks.len(), 1);
    runtime
        .invoke_microtask(&mut parsed.dom, tasks.into_iter().next().unwrap())
        .expect("observer delivery should execute");
    assert_eq!(
        parsed.dom.attribute(image, "src").unwrap(),
        Some("loaded.png")
    );
    let outcome = runtime.execute(&mut parsed.dom, "observed").unwrap();
    assert_eq!(outcome.value, JsValue::String("true:1:30".to_owned()));
}

#[test]
fn caught_native_errors_materialize_as_standard_error_instances() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                var report = '';
                try { (null).foo; } catch (e) {
                    report += (e instanceof TypeError) + ':';
                    report += (e.message === "Cannot read properties of null (reading 'foo')") + ':';
                    report += typeof e.stack;
                }
                report;
            "#,
        )
        .expect("catch executes");
    assert_eq!(
        outcome.value,
        JsValue::String("true:true:string".to_owned())
    );
}

#[test]
fn error_stack_names_active_frames_and_matches_header() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                function outer() {
                    return new TypeError('boom').stack;
                }
                var stack = outer();
                var ok = stack.indexOf('TypeError: boom') === 0;
                ok = ok && stack.indexOf('    at outer') !== -1;
                ok + ':' + stack;
            ",
        )
        .expect("stack capture executes");
    let JsValue::String(stack) = outcome.value else {
        panic!("stack should be a string");
    };
    let (flag, text) = stack.split_once(':').expect("flag:stack");
    assert_eq!(flag, "true");
    assert!(text.contains("TypeError: boom"), "stack: {text}");
    assert!(text.contains("    at outer"), "stack: {text}");
}

#[test]
fn anonymous_frames_get_stable_labels() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var stack = '';
                (function () { stack = new RangeError('r').stack; })();
                stack.indexOf('    at <anonymous fn #') !== -1;
            ",
        )
        .expect("anonymous frame executes");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn runtime_errors_report_source_line_and_column() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let source = "var ok = 1;\nok = 2;\nvar broken = (null).prop;\n";
    let error = runtime
        .execute(&mut parsed.dom, source)
        .expect_err("member read on null throws");
    // The positioned node is the member access itself (the `.` token).
    assert_eq!(error.position(), Some((3, 21)));
    assert!(
        error.to_string().contains("at line 3, column 21"),
        "display: {error}"
    );
}

#[test]
fn thrown_values_surface_positions_without_offsets() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let source = "var prepared = true;\nthrow new TypeError('late');\n";
    let error = runtime
        .execute(&mut parsed.dom, source)
        .expect_err("throw escapes");
    // The throw statement's expression carries a span; position resolves.
    assert!(error.to_string().contains("line 2"), "display: {error}");
}

#[test]
fn accessor_properties_invoke_getters_and_setters_through_the_chain() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var stored = 0;
                var recorded = '';
                var base = {};
                Object.defineProperty(base, 'size', {
                    get: function () { return stored * 2; },
                    set: function (v) { stored = v; recorded += 's'; },
                    configurable: true,
                });
                var child = Object.create(base);
                child.size = 21;
                var read = child.size;
                read + ':' + recorded + ':' + stored;
            ",
        )
        .expect("accessor chain executes");
    assert_eq!(
        outcome.value,
        JsValue::String("42:s:21".to_owned()),
        "setter fires on the receiver, getter multiplies stored"
    );
}

#[test]
fn object_get_own_property_descriptor_reports_accessors() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var o = {};
                Object.defineProperty(o, 'x', { get: function () { return 7; } });
                var d = Object.getOwnPropertyDescriptor(o, 'x');
                [typeof d.get, d.set === undefined, d.enumerable, d.configurable === false, 'value' in d].join(',');
            ",
        )
        .expect("descriptor introspection executes");
    // Spec: attributes omitted from the descriptor default to false.
    assert_eq!(
        outcome.value,
        JsValue::String("function,true,false,true,false".to_owned())
    );
}

#[test]
fn accessor_descriptor_with_value_is_rejected() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let error = runtime
        .execute(
            &mut parsed.dom,
            "Object.defineProperty({}, 'x', { get: function () {}, value: 1 });",
        )
        .expect_err("mixed accessor and value descriptor is invalid");
    assert!(error.to_string().contains("Type"), "{error}");
}

#[test]
fn object_literal_accessors_and_define_getter_family_agree() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var o = {
                    inner: 5,
                    get doubled() { return this.inner * 2; },
                };
                var results = [];
                results.push(o.doubled === 10);
                __defineGetter__.call(o, 'tripled', function () { return this.inner * 3; });
                results.push(o.tripled === 15);
                var report = results.join(',') + ':' + typeof __lookupGetter__.call(o, 'tripled');
                report;
            ",
        )
        .expect("accessor literal executes");
    assert_eq!(
        outcome.value,
        JsValue::String("true,true:function".to_owned())
    );
}

#[test]
fn object_integrity_levels_freeze_seal_and_extension_guards() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var report = [];
                var frozen = Object.freeze({ keep: 1 });
                report.push(Object.isFrozen(frozen));
                report.push(Object.isExtensible(frozen) === false);
                frozen.keep = 2;
                report.push(frozen.keep === 1);
                report.push(Object.isSealed(Object.seal({ x: 1 })));
                var open = Object.preventExtensions({ y: 1 });
                report.push(Object.isExtensible(open) === false);
                var grew = false;
                try { open.z = 1; grew = open.z === 1; } catch (e) { grew = false; }
                report.push(grew === false);
                report.join(',');
            ",
        )
        .expect("integrity builtins execute");
    assert_eq!(
        outcome.value,
        JsValue::String("true,true,true,true,true,true".to_owned())
    );
}

#[test]
fn own_string_keys_enumerate_in_insertion_order() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var literal = { second: 1, first: 2 };
                literal.third = 3;
                Object.getOwnPropertyNames(literal).join(',') + ':' + Object.keys(literal).join(',');
            ",
        )
        .expect("ordering probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String("second,first,third:second,first,third".to_owned())
    );
}

#[test]
fn symbols_are_real_primitives_with_spec_identity() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                var a = Symbol('x');
                var b = Symbol('x');
                results.push(typeof a === 'symbol');
                results.push(a !== b);
                results.push(a.description === 'x');
                results.push(Symbol.for('k') === Symbol.for('k'));
                results.push(Symbol.keyFor(a) === undefined);
                var reg = Symbol.for('reg');
                results.push(Symbol.keyFor(reg) === 'reg');
                results.push(String(a) === 'Symbol(x)');
                results.join(',');
            ",
        )
        .expect("symbol probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String("true,true,true,true,true,true,true".to_owned())
    );
}

#[test]
fn new_symbol_throws_and_well_knowns_are_symbol_values() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                try { new Symbol('x'); results.push('no-throw'); }
                catch (e) { results.push(e instanceof TypeError); }
                results.push(typeof Symbol.iterator === 'symbol');
                results.push(Symbol.toStringTag.toString() === 'Symbol(@@toStringTag)');
                results.join(',');
            ",
        )
        .expect("well-known probe executes");
    assert_eq!(outcome.value, JsValue::String("true,true,true".to_owned()));
}

#[test]
fn symbol_keyed_properties_round_trip_through_brackets_and_introspection() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var k = Symbol('k');
                var o = {};
                o[k] = 42;
                var results = [];
                results.push(o[k] === 42);
                results.push(Object.getOwnPropertySymbols(o).length === 1);
                results.push(Object.getOwnPropertySymbols(o)[0] === k);
                results.push(k in o);
                results.push(Object.keys(o).length === 0);
                delete o[k];
                results.push(o[k] === undefined);
                var inherited = {};
                inherited[Symbol.for('tag')] = 'kept';
                var child = Object.create(inherited);
                results.push(child[Symbol.for('tag')] === 'kept');
                results.join(',');
            ",
        )
        .expect("symbol property probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String("true,true,true,true,true,true,true".to_owned())
    );
}

#[test]
fn for_of_and_spread_drive_map_set_and_custom_iterators() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                var map = new Map();
                map.set('a', 1); map.set('b', 2);
                var pairs = [];
                for (const entry of map) { pairs.push(entry[0] + '=' + entry[1]); }
                results.push(pairs.join('|') === 'a=1|b=2');
                var set = new Set();
                set.add('x'); set.add('y');
                var seen = [];
                for (const item of set) { seen.push(item); }
                results.push(seen.join('') === 'xy');
                var custom = { count: 3, current: 0, next: function () {
                    this.current += 1;
                    return this.current > this.count ? { done: true } : { done: false, value: this.current };
                } };
                custom[Symbol.iterator] = function () { return { next: custom.next.bind(custom) }; };
                var gathered = [];
                for (const n of custom) { gathered.push(n); }
                results.push(gathered.join(',') === '1,2,3');
                function take() { return arguments.length; }
                var list = ['p', 'q'];
                results.push(take(...list, 'r') === 3);
                var fresh = { count: 2, current: 0, next: function () {
                    this.current += 1;
                    return this.current > this.count ? { done: true } : { done: false, value: this.current * 10 };
                } };
                fresh[Symbol.iterator] = function () { return { next: fresh.next.bind(fresh) }; };
                var copy = [...seen, ...fresh];
                results.push(copy.join(',') === 'x,y,10,20');
                results.join(',');
            ",
        )
        .expect("iterator probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String("true,true,true,true,true".to_owned())
    );
}

#[test]
fn to_primitive_and_has_instance_hooks_participate() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                var boxed = { toString: function () { return 'plain'; } };
                boxed[Symbol.toPrimitive] = function (hint) { return 'prim:' + hint; };
                results.push(String(boxed) === 'prim:string');
                results.push(boxed * 1 === 'prim:default' * 1 || typeof (boxed * 1) === 'number');
                results.push(`${boxed}` === 'prim:default');
                classLike = function () {};
                classLike[Symbol.hasInstance] = function (v) { return v === 'member'; };
                results.push('member' instanceof classLike);
                results.push(!('other' instanceof classLike));
                results.join(',');
            ",
        )
        .expect("hook probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String("true,true,true,true,true".to_owned())
    );
}

#[test]
fn promise_prototype_is_real_and_overridable() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                results.push(typeof Promise.prototype.then === 'function');
                results.push(Promise.prototype.toString() === '[object Promise]');
                var calls = 0;
                var original = Promise.prototype.then;
                Promise.prototype.then = function (a, b) {
                    calls += 1;
                    return original.call(this, a, b);
                };
                Promise.resolve(5).then(function (v) { results.push(v === 5); });
                Promise.resolve('x').finally(function () { calls += 10; });
                'setup-done';
            ",
        )
        .expect("promise prototype probe executes");
    // Settlement callbacks run at the microtask checkpoint, after the
    // script; drain pending microtasks before reading the results.
    for microtask in runtime.take_pending_microtasks() {
        runtime
            .invoke_microtask(&mut parsed.dom, microtask)
            .expect("microtask executes");
    }
    let outcome = runtime
        .execute(&mut parsed.dom, "results.join(',') + ':' + (calls === 11)")
        .expect("results read executes");
    assert_eq!(
        outcome.value,
        JsValue::String("true,true,true:true".to_owned())
    );
}

#[test]
fn temp_diag_mutual_recursion() {
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(|| {
            let Ok(html) = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../.diag/bilibili/page.html"
            )) else {
                eprintln!("skipped: saved bilibili page not present");
                return;
            };
            let mut parsed = parse_document(&html);
            let url = Url::parse("https://www.bilibili.com/").expect("base URL");
            let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
            let shim = r"
                globalThis.__dpErr = 'none';
                var __origDP = Object.defineProperty;
                Object.defineProperty = function (target, key, desc) {
                    try {
                        return __origDP.call(Object, target, key, desc);
                    } catch (e) {
                        if (globalThis.__dpErr === 'none') { globalThis.__dpErr = '' + e; }
                        throw e;
                    }
                };
            ";
            runtime.execute(&mut parsed.dom, shim).expect("shim");
            let pre = runtime
                .execute(
                    &mut parsed.dom,
                    r"var n = {}; n[Symbol.toStringTag] = 'z';
                       [String(n), n[Symbol.toStringTag], Object.prototype.toString.call(n)].join('|');
                    ",
                )
                .map(|o| o.value.to_js_string())
                .unwrap_or_else(|e| format!("pre failed: {e}"));
            eprintln!("PRE PROBE: {pre}");
            let source = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../.diag/bilibili/assets/a001_log-reporter.js"
            ))
            .expect("log-reporter source");
            match runtime.execute(&mut parsed.dom, &source) {
                Ok(_) => eprintln!("NO ERROR"),
                Err(error) => {
                    eprintln!("ERR: {error}");
                    for index in [668usize, 692] {
                        let Some(function) = runtime.functions.get(index) else {
                            eprintln!("fn #{index}: missing");
                            continue;
                        };
                        let body = format!("{:?}", function.body);
                        let body = if body.len() > 700 {
                            format!("{}…", &body[..700])
                        } else {
                            body
                        };
                        eprintln!(
                            "fn #{index} name={:?} params={:?} body={body}",
                            function.name, function.parameters
                        );
                    }
                    let report = runtime
                        .execute(&mut parsed.dom, "globalThis.__dpErr")
                        .map(|o| o.value.to_js_string())
                        .unwrap_or_else(|e| format!("report failed: {e}"));
                    eprintln!("DP ERR: {report}");
                }
            }
        })
        .expect("spawn");
    handle.join().expect("join");
}

#[test]
fn fetch_queues_exactly_one_pending_request_with_method_headers_and_body() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                var promise = fetch("https://example.test/api", {
                    method: "POST",
                    headers: { "Content-Type": "application/json", "X-Trace": "7" },
                    body: "{\"user\":\"zdy\"}"
                });
                typeof promise.then;
            "#,
        )
        .expect("fetch call should execute");
    assert_eq!(outcome.value, JsValue::String("function".to_owned()));

    let mut requests = runtime.take_pending_fetch_requests();
    assert_eq!(requests.len(), 1);
    let request = requests.pop().expect("one pending fetch");
    assert_eq!(request.url, "https://example.test/api");
    assert_eq!(request.method, "POST");
    assert!(
        request
            .headers
            .iter()
            .any(|(name, value)| name == "Content-Type" && value == "application/json")
    );
    assert!(
        request
            .headers
            .iter()
            .any(|(name, value)| name == "X-Trace" && value == "7")
    );
    assert_eq!(request.body.as_deref(), Some("{\"user\":\"zdy\"}"));

    // Draining consumed the queue; re-draining stays empty.
    assert!(runtime.take_pending_fetch_requests().is_empty());
}

#[test]
fn fetch_resolves_relative_urls_against_the_document_base() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let url = Url::parse("https://example.test/app/").expect("test URL");
    let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
    runtime
        .execute(&mut parsed.dom, r#"fetch("api/v1?id=7");"#)
        .expect("relative fetch should execute");
    let requests = runtime.take_pending_fetch_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://example.test/app/api/v1?id=7");
    assert_eq!(requests[0].method, "GET");
}

#[test]
fn settle_fetch_ok_resolves_response_text_through_microtasks() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                var result = "";
                fetch("https://example.test/data")
                    .then(function (response) {
                        result += response.status + ":" + response.ok + ":" +
                            response.statusText + ":" +
                            response.headers.get("content-type") + ";";
                        return response.text();
                    })
                    .then(function (text) { result += text; });
            "#,
        )
        .expect("fetch chain should execute");
    let requests = runtime.take_pending_fetch_requests();
    assert_eq!(requests.len(), 1);
    let id = requests[0].id;

    runtime.settle_fetch(
        &mut parsed.dom,
        id,
        Ok(FetchOutcome {
            status: 200,
            status_text: "OK".to_owned(),
            headers: vec![("Content-Type".to_owned(), "text/plain".to_owned())],
            body: b"hello fetch".to_vec(),
        }),
    );
    drain_microtasks(&mut runtime, &mut parsed.dom);
    let outcome = runtime
        .execute(&mut parsed.dom, "result")
        .expect("result read executes");
    assert_eq!(
        outcome.value,
        JsValue::String("200:true:OK:text/plain;hello fetch".to_owned())
    );
    // Settled ids leave no bookkeeping behind.
    assert!(runtime.take_pending_fetch_requests().is_empty());
}

#[test]
fn settle_fetch_error_rejects_and_catch_receives_the_reason() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                var reason = "";
                fetch("https://example.test/data").catch(function (error) {
                    reason = error.message;
                });
            "#,
        )
        .expect("fetch chain should execute");
    let requests = runtime.take_pending_fetch_requests();
    assert_eq!(requests.len(), 1);
    let id = requests[0].id;

    // Unknown ids are ignored silently (the page may have navigated).
    runtime.settle_fetch(
        &mut parsed.dom,
        u64::MAX,
        Ok(FetchOutcome {
            status: 200,
            status_text: "OK".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        }),
    );

    runtime.settle_fetch(
        &mut parsed.dom,
        id,
        Err("host name lookup failed".to_owned()),
    );
    drain_microtasks(&mut runtime, &mut parsed.dom);
    let outcome = runtime
        .execute(&mut parsed.dom, "reason")
        .expect("reason read executes");
    assert_eq!(
        outcome.value,
        JsValue::String("host name lookup failed".to_owned())
    );
}

#[test]
fn xhr_queues_request_and_completes_with_events_status_and_response_text() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r##"
                var log = "";
                var xhr = new XMLHttpRequest();
                xhr.open("POST", "https://example.test/login");
                xhr.setRequestHeader("Content-Type", "application/x-www-form-urlencoded");
                xhr.setRequestHeader("X-Request-Id", "42");
                xhr.onreadystatechange = function () {
                    if (xhr.readyState === 4) log += "rs" + xhr.status + ":" + xhr.responseText;
                };
                xhr.onload = function (event) { log += "load" + (event.target === xhr) + event.type; };
                xhr.onerror = function () { log += "error"; };
                xhr.onloadend = function () { log += "|end"; };
                var headerBeforeSend = xhr.getResponseHeader("X-Session");
                xhr.send("user=a&pass=b");
                log + "#" + headerBeforeSend;
            "##,
        )
        .expect("XHR setup should execute");
    let mut requests = runtime.take_pending_fetch_requests();
    assert_eq!(requests.len(), 1);
    let request = requests.pop().expect("one pending fetch");
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://example.test/login");
    assert!(request.headers.iter().any(
        |(name, value)| name == "Content-Type" && value == "application/x-www-form-urlencoded"
    ));
    assert!(
        request
            .headers
            .iter()
            .any(|(name, value)| name == "X-Request-Id" && value == "42")
    );
    assert_eq!(request.body.as_deref(), Some("user=a&pass=b"));

    runtime.settle_fetch(
        &mut parsed.dom,
        request.id,
        Ok(FetchOutcome {
            status: 200,
            status_text: "OK".to_owned(),
            headers: vec![("X-Session".to_owned(), "abc".to_owned())],
            body: b"welcome".to_vec(),
        }),
    );
    drain_microtasks(&mut runtime, &mut parsed.dom);

    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r##"
                log + "#" + xhr.getResponseHeader("x-session") + "#" +
                    xhr.readyState + "," + xhr.status + "," + xhr.statusText;
            "##,
        )
        .expect("XHR result read executes");
    // readystatechange, load, then loadend, all observing the settled state.
    assert_eq!(
        outcome.value,
        JsValue::String("rs200:welcomeloadtrueload|end#abc#4,200,OK".to_owned())
    );
}

#[test]
fn xhr_transport_failure_fires_error_with_status_zero() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                var log = "";
                var xhr = new XMLHttpRequest();
                xhr.open("GET", "https://example.test/offline");
                xhr.onreadystatechange = function () {
                    if (xhr.readyState === 4) log += "rs" + xhr.status;
                };
                xhr.onload = function () { log += "load"; };
                xhr.onerror = function () { log += "error"; };
                xhr.send();
            "#,
        )
        .expect("XHR setup should execute");
    let id = runtime.take_pending_fetch_requests()[0].id;
    runtime.settle_fetch(&mut parsed.dom, id, Err("request timed out".to_owned()));
    drain_microtasks(&mut runtime, &mut parsed.dom);
    let outcome = runtime
        .execute(&mut parsed.dom, "log + '#' + xhr.status")
        .expect("XHR log read executes");
    assert_eq!(outcome.value, JsValue::String("rs0error#0".to_owned()));
}

#[test]
fn xhr_synchronous_send_throws_and_queues_nothing() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                var report = "";
                try {
                    var sync = new XMLHttpRequest();
                    sync.open("GET", "https://example.test/sync", false);
                    sync.send();
                    report = "sent";
                } catch (error) {
                    report = error instanceof TypeError ? "TypeError" : "other";
                }
                report;
            "#,
        )
        .expect("sync XHR probe should execute");
    assert_eq!(outcome.value, JsValue::String("TypeError".to_owned()));
    assert!(runtime.take_pending_fetch_requests().is_empty());
}

#[test]
fn response_json_parses_the_body_into_a_readable_object() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                var parsed = null;
                fetch("https://example.test/api")
                    .then(function (response) { return response.json(); })
                    .then(function (value) { parsed = value; });
            "#,
        )
        .expect("fetch chain should execute");
    let id = runtime.take_pending_fetch_requests()[0].id;
    runtime.settle_fetch(
        &mut parsed.dom,
        id,
        Ok(FetchOutcome {
            status: 200,
            status_text: "OK".to_owned(),
            headers: Vec::new(),
            body: br#"{"user":"zdy","count":2,"nested":{"ok":true}}"#.to_vec(),
        }),
    );
    drain_microtasks(&mut runtime, &mut parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            "parsed.user + ':' + parsed.count + ':' + parsed.nested.ok",
        )
        .expect("parsed read executes");
    assert_eq!(outcome.value, JsValue::String("zdy:2:true".to_owned()));
}

#[test]
fn response_constructor_text_settles_without_a_transfer() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                var parts = [];
                var response = new Response("ready");
                parts.push(response.status + "," + response.ok);
                response.text().then(function (text) { parts.push(text); });
            "#,
        )
        .expect("Response construction should execute");
    assert!(runtime.take_pending_fetch_requests().is_empty());
    drain_microtasks(&mut runtime, &mut parsed.dom);
    let outcome = runtime
        .execute(&mut parsed.dom, "parts.join('#')")
        .expect("parts read executes");
    assert_eq!(outcome.value, JsValue::String("200,true#ready".to_owned()));
}

#[test]
fn temp_read_5073_state() {
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(|| {
            let Ok(html) = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../.diag/bilibili/page.html"
            )) else {
                eprintln!("skipped: saved bilibili page not present");
                return;
            };
            let mut parsed = parse_document(&html);
            let url = Url::parse("https://www.bilibili.com/").expect("base URL");
            let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
            let shim = r"
                globalThis.__dpErr = 'none';
                var __origDP = Object.defineProperty;
                Object.defineProperty = function (target, key, desc) {
                    try { return __origDP.call(Object, target, key, desc); }
                    catch (e) { throw e; }
                };
            ";
            runtime.execute(&mut parsed.dom, shim).expect("shim");
            let source = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../.diag/bilibili/assets/a001_patched.js"
            ))
            .expect("patched bundle");
            match runtime.execute(&mut parsed.dom, &source) {
                Ok(_) => eprintln!("NO ERROR"),
                Err(error) => eprintln!("ERR: {error}"),
            }
            let report = runtime
                .execute(
                    &mut parsed.dom,
                    r"var t = {}; t[Symbol.toStringTag] = 'z';
                       [typeof Symbol, typeof Symbol.toStringTag, String(t), String({})].join(' ; ');
                    ",
                )
                .map(|o| o.value.to_js_string())
                .unwrap_or_else(|e| format!("report failed: {e}"));
            eprintln!("5073 STATE: {report}");
        })
        .expect("spawn");
    handle.join().expect("join");
}

#[test]
fn dataset_setter_writes_through_to_the_data_attribute_and_reads_back() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var script = document.createElement("script");
                    script.dataset.sentryConfig = "https://sentry.test/42";
                    [
                        script.dataset.sentryConfig,
                        script.getAttribute("data-sentry-config")
                    ].join("|");
                "#,
        )
        .expect("dataset member write should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("https://sentry.test/42|https://sentry.test/42".to_owned())
    );
}

#[test]
fn dataset_reads_delete_and_missing_members_follow_the_camel_case_mapping() {
    let mut parsed =
        parse_document("<!doctype html><p id='probe' data-foo-bar='first' data-num='7'></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var probe = document.getElementById("probe");
                    var before = [probe.dataset.fooBar, probe.dataset.num, probe.dataset.missing].join("|");
                    delete probe.dataset.fooBar;
                    var after = [
                        probe.dataset.fooBar,
                        probe.hasAttribute("data-foo-bar"),
                        probe.getAttribute("data-num")
                    ].join("|");
                    before + " / " + after;
                "#,
        )
        .expect("dataset member reads should execute");
    // Spec: Array.prototype.join renders undefined members as empty
    // strings, so the missing/deleted reads join as "".
    assert_eq!(
        outcome.value,
        JsValue::String("first|7| / |false|7".to_owned())
    );
}

#[test]
fn user_functions_expose_name_and_length_own_properties() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    function declared(w, x, y = 4, ...z) { return [w, x, y, z].length; }
                    var anonymous = function (a, b) {};
                    var arrow = (p, q = 1, ...r) => p;
                    var arrowDefaultPattern = ({x} = {}) => x;
                    var results = [
                        declared.name, declared.length,
                        anonymous.name, anonymous.length,
                        arrow.name, arrow.length,
                        arrowDefaultPattern.length,
                        typeof (function () {}).name,
                        declared.hasOwnProperty("name"),
                        declared.hasOwnProperty("length"),
                        Object.keys(declared).length
                    ].join("|");
                    declared.name = "renamed";
                    results + "|" + declared.name;
                "#,
        )
        .expect("function metadata probe should execute");
    assert_eq!(
        outcome.value,
        JsValue::String(
            // `length` counts parameters before the first default and
            // excludes the rest parameter; anonymous callables read "".
            "declared|2||2||1|0|string|true|true|0|declared".to_owned()
        )
    );
}

#[test]
fn default_and_rest_parameters_still_bind_argument_positions() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    function probe(a, b = 5, ...rest) {
                        return [a, b, typeof rest].join("|");
                    }
                    probe(1, 2, 3, 4) + " / " + probe(7);
                "#,
        )
        .expect("default/rest parameter call should execute");
    // Default initializers are not applied by this runtime yet (parameters
    // bind positionally, rest included), so the metadata markers must leave
    // argument positions unchanged.
    assert_eq!(
        outcome.value,
        // `Array.prototype.join` renders undefined members as "".
        JsValue::String("1|2|number / 7||undefined".to_owned())
    );
}

#[test]
fn bound_functions_take_the_bound_name_and_shrunk_length() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    function target(a, b, c) {}
                    var partial = target.bind(null, 1);
                    var total = target.bind(null);
                    [partial.name, partial.length, total.name, total.length].join("|");
                "#,
        )
        .expect("bound function metadata probe should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("bound target|2|bound target|3".to_owned())
    );
}

#[test]
fn uncaught_script_errors_dispatch_a_window_error_event() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let url = Url::parse("https://example.test/app.js").expect("test URL");
    let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                    window.errors = [];
                    window.addEventListener("error", function (event) {
                        window.errors.push([
                            event.type,
                            event.message,
                            event.filename,
                            event.lineno + ":" + event.colno,
                            event.error instanceof Error ? "real-error" : "no-error"
                        ].join("|"));
                    });
                "#,
        )
        .expect("error listener registration should execute");
    let error = runtime
        .execute(&mut parsed.dom, "boom();")
        .expect_err("uncaught script failure must still surface to the embedder");
    assert_eq!(error.kind(), crate::js::JsErrorKind::Reference);
    // `try`/`catch` contains its own throw, so no additional event fires.
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    try { missing(); } catch (error) { window.handled = true; }
                    [window.errors.length, window.errors[0], window.handled === true].join("|");
                "#,
        )
        .expect("post-error probe should execute");
    assert_eq!(
        outcome.value,
        JsValue::String(
            "1|error|boom is not defined|https://example.test/app.js|1:1|real-error|true"
                .to_owned()
        )
    );
}

#[test]
fn uncaught_microtask_errors_dispatch_a_window_error_event() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                    window.failures = [];
                    window.addEventListener("error", function (event) {
                        window.failures.push(
                            event.message + ":" +
                            (event.error instanceof TypeError ? "typed" : "other")
                        );
                    });
                    queueMicrotask(function () {
                        throw new TypeError("microtask boom");
                    });
                "#,
        )
        .expect("microtask scheduling should execute");
    let pending = runtime.take_pending_microtasks();
    assert_eq!(pending.len(), 1);
    // The exception escaping the microtask still propagates to the
    // embedding, which now also observed the window `error` event.
    let error = runtime
        .invoke_microtask(&mut parsed.dom, pending[0].clone())
        .expect_err("uncaught microtask failure must still surface to the embedder");
    assert_eq!(error.kind(), crate::js::JsErrorKind::Throw);
    let outcome = runtime
        .execute(&mut parsed.dom, "window.failures.join(\";\");")
        .expect("failure probe should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("microtask boom:typed".to_owned())
    );
}
