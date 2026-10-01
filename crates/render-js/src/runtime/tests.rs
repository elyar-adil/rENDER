use std::collections::BTreeMap;

use crate::JsValue;
use crate::{ElementRect, FetchOutcome, JsRuntime};
use render_html::parse_document;
use url::Url;

/// Run every queued microtask (including ones queued by earlier microtasks)
/// until the runtime has none left.
fn drain_microtasks(runtime: &mut JsRuntime, dom: &mut render_dom::Dom) {
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
            crate::NavigationRequest {
                url: "https://example.test/next".to_owned(),
                replace: false
            },
            crate::NavigationRequest {
                url: "https://other.test/a".to_owned(),
                replace: false
            },
            crate::NavigationRequest {
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
    assert_eq!(error.kind(), crate::JsErrorKind::Type);
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
                    var container = document.getElementById("parent");
                    var child = document.getElementById("child");
                    var observed = false;
                    function listener(event) {
                        observed = event.target === child &&
                            event.currentTarget === container && this === container;
                    }
                    container.addEventListener("activate", listener);
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
fn proxy_traps_forward_real_world_reactive_access() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                var target = { count: 1 };
                var reads = 0;
                var proxy = new Proxy(target, {
                    get: function(object, key, receiver) {
                        reads += 1;
                        return Reflect.get(object, key, receiver);
                    },
                    set: function(object, key, value, receiver) {
                        return Reflect.set(object, key, value, receiver);
                    },
                    has: function(object, key) { return Reflect.has(object, key); },
                    deleteProperty: function(object, key) { return Reflect.deleteProperty(object, key); },
                    ownKeys: function(object) { return Reflect.ownKeys(object); }
                });
                proxy.count = proxy.count + 1;
                var keys = Object.keys(proxy);
                ("count" in proxy) && delete proxy.count && target.count === undefined && reads > 0 && keys[0] === "count";
            "#,
        )
        .expect("Proxy traps should execute");
    assert_eq!(outcome.value, JsValue::Boolean(true));
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
fn document_create_event_returns_timestamped_event() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                var event = document.createEvent("Event");
                typeof event === "object" && typeof event.timeStamp === "number" &&
                    event.timeStamp >= 0;
            "#,
        )
        .expect("document.createEvent should produce a generic Event");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn browser_constructor_aliases_and_screen_metrics_are_available() {
    let mut parsed = parse_document("<!doctype html><main></main>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                [typeof Document, typeof HTMLDocument, typeof DocumentFragment,
                 document instanceof Document, screen.width, screen.height,
                 screen.colorDepth, innerWidth > 0, innerHeight > 0].join("|");
            "#,
        )
        .expect("browser globals should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("function|function|function|true|1024|768|24|true|true".to_owned())
    );
}

#[test]
fn base64_globals_support_media_bootstrap_decoding() {
    let mut parsed = parse_document("<!doctype html><main></main>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(&mut parsed.dom, r#"btoa("render");"#)
        .expect("btoa should execute");
    assert_eq!(outcome.value, JsValue::String("cmVuZGVy".to_owned()));
    let round_trip = runtime
        .execute(&mut parsed.dom, r#"atob("cmVuZGVy");"#)
        .expect("atob should execute");
    assert_eq!(round_trip.value, JsValue::String("render".to_owned()));
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
        vec![crate::TimerRequest::Schedule {
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
        crate::ConsoleLevel::Log.label(),
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
        [crate::TimerRequest::Schedule { delay_ms: 0.0, .. }]
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
    let limits = crate::RuntimeLimits {
        max_dom_nodes_created: 2,
        ..crate::RuntimeLimits::default()
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
    assert_eq!(error.kind(), crate::JsErrorKind::ResourceLimit);
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
    let body = crate::runtime::builtins::dom::find_body_node(&parsed.dom, parsed.dom.document())
        .expect("body exists");
    let paragraph = parsed.dom.children(body).unwrap_or_default()[0];
    geometry.insert(
        paragraph.as_u64(),
        crate::ElementRect {
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
            crate::runtime::builtins::dom::find_body_node(&parsed.dom, parsed.dom.document())
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
            crate::runtime::builtins::dom::find_body_node(&parsed.dom, parsed.dom.document())
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
fn to_object_boxes_primitives_into_distinct_wrappers() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                results.push(Object.getPrototypeOf('') === String.prototype);
                results.push(Object.getPrototypeOf(0) === Number.prototype);
                results.push(Object.getPrototypeOf(false) === Boolean.prototype);
                results.push(Object.getPrototypeOf(Symbol('s')) === Symbol.prototype);
                // ToObject does not cache: two boxes are two objects.
                results.push(Object(1) !== Object(1));
                results.push(Object.getPrototypeOf(Object(1)) === Number.prototype);
                // A Number wrapper owns nothing; valueOf/toString live on the
                // prototype, and `instanceof` works through the same chain.
                var boxed = Object(1);
                results.push(Object.getOwnPropertyNames(boxed).length === 0);
                results.push(Object.getOwnPropertyDescriptor(boxed, 'toString') === undefined);
                results.push(typeof boxed.toString === 'function' && boxed.toString() === '1');
                results.push(boxed.valueOf() === 1);
                results.push(boxed instanceof Number);
                results.push(Object(1) instanceof Number);
                // Symbol wrappers too.
                var sym = Object(Symbol('q'));
                results.push(typeof sym.toString === 'function' && sym.toString() === 'Symbol(q)');
                results.push(typeof sym.valueOf() === 'symbol');
                results.join(',');
            ",
        )
        .expect("ToObject probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String(
            "true,true,true,true,true,true,true,true,true,true,true,true,true,true".to_owned()
        )
    );
}

#[test]
fn to_object_on_nullish_throws_while_strings_expose_exotic_own_keys() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                function thrown(fn) { try { fn(); return false; } catch (e) { return e instanceof TypeError; } }
                results.push(thrown(function () { Object.getPrototypeOf(null); }));
                results.push(thrown(function () { Object.getPrototypeOf(undefined); }));
                results.push(thrown(function () { Object.keys(1, 2); }));
                // A String wrapper's characters and length are own properties
                // with the spec's attributes.
                results.push(Object.getOwnPropertyNames('ab').join(','));
                results.push(Object.keys('ab').join(','));
                var indexed = Object.getOwnPropertyDescriptor('ab', '0');
                results.push([indexed.value, indexed.writable, indexed.enumerable, indexed.configurable].join(','));
                var length = Object.getOwnPropertyDescriptor('ab', 'length');
                results.push([length.value, length.writable, length.enumerable, length.configurable].join(','));
                results.push(Object.hasOwn('ab', '0'));
                results.push('ab'.hasOwnProperty('1'));
                results.push(Object.values('ab').join(','));
                // Ordinary properties are still storable, and the virtual
                // slots reject redefinition.
                var wrapper = Object('ab');
                wrapper.tag = 'x';
                results.push(Object.getOwnPropertyNames(wrapper).join(','));
                results.push(thrown(function () { Object.defineProperty(wrapper, '0', { value: 'z' }); }));
                results.push(thrown(function () { Object.defineProperty(wrapper, 'length', { value: 0 }); }));
                results.push(wrapper[0] + wrapper.length + wrapper.tag);
                results.join(',');
            ",
        )
        .expect("String exotic probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String(
            "true,true,false,0,1,length,0,1,a,false,true,false,2,false,false,false,true,true,a,b,0,1,tag,length,true,true,a2x"
                .to_owned()
        )
    );
}

#[test]
fn object_statics_and_reflect_share_one_to_object() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                // Reflect builtins all start with ToObject (§27.1), so a
                // primitive target is its wrapper.
                results.push(Reflect.getPrototypeOf('') === String.prototype);
                results.push(Reflect.getPrototypeOf(1) === Number.prototype);
                results.push(Reflect.get(1, 'toFixed') === Number.prototype.toFixed);
                results.push(Reflect.get(1, 'toFixed').call(1, 2) === '1.00');
                results.push(Reflect.has('ab', '0'));
                results.push(Reflect.ownKeys('ab').join(','));
                results.push(Reflect.isExtensible({}) === true);
                results.push(Reflect.preventExtensions(Object(1)) === true);
                results.push(Reflect.apply(Math.max, null, [1, 5, 2]) === 5);
                results.push(typeof Reflect.setPrototypeOf === 'function');
                // `Reflect.setPrototypeOf` and the `newTarget` check stay strict.
                function thrown(fn) { try { fn(); return false; } catch (e) { return e instanceof TypeError; } }
                results.push(thrown(function () { Reflect.setPrototypeOf(1, null); }));
                results.push(thrown(function () { Reflect.construct(function () {}, [], 1); }));
                // Object statics that ToObject first.
                results.push(Object.getOwnPropertyDescriptor(1, 'x') === undefined);
                results.push(Object.hasOwn('', 'length'));
                results.push(Object.getOwnPropertySymbols(Symbol('a')).length === 0);
                var frozen = Object.freeze(1);
                results.push(frozen instanceof Number && Object.isFrozen(frozen));
                results.push(typeof Object.defineProperty(1, 'x', { value: 1 }));
                results.push(Object.getOwnPropertyNames(Object.defineProperty(1, 'x', { value: 1 })).join(','));
                var target = Object.assign(1, { a: 2 });
                results.push([target instanceof Number, target.a, Object.keys(target).join(',')].join('/'));
                // Integrity queries on a nullish target.
                results.push(Object.isFrozen(null));
                results.push(Object.isSealed(undefined));
                results.push(thrown(function () { Object.isExtensible(null); }));
                results.push(thrown(function () { Object.freeze(null); }));
                // Operations that must keep throwing on non-objects.
                results.push(thrown(function () { new Proxy(1, {}); }));
                // `OrdinaryHasInstance` answers false for a primitive left-hand
                // side rather than throwing, so only a non-object right-hand
                // side is a TypeError.
                results.push((1 instanceof Number) === false);
                results.push(thrown(function () { return 'a' in 5; }));
                results.push(thrown(function () { return new Set(5); }));
                results.join(',');
            ",
        )
        .expect("Object/Reflect probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String(
            "true,true,true,true,true,0,1,length,true,true,true,true,true,true,true,true,true,true,object,x,true/2/a,true,true,true,true,true,true,true,true"
                .to_owned()
        )
    );
}

#[test]
fn proto_accessor_and_object_literal_proto_set_the_prototype() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                var descriptor = Object.getOwnPropertyDescriptor(Object.prototype, '__proto__');
                results.push(typeof descriptor.get === 'function' && typeof descriptor.set === 'function');
                results.push([descriptor.enumerable, descriptor.configurable].join(','));
                results.push(({}).__proto__ === Object.prototype);
                results.push([].__proto__ === Array.prototype);
                results.push(''.__proto__ === String.prototype);
                results.push((5).__proto__ === Number.prototype);
                results.push(Object.create(null).__proto__ === undefined);
                // Assignment writes the prototype, and object literals use the
                // `__proto__: value` special form.
                var target = { a: 1 };
                var base = { b: 2 };
                target.__proto__ = base;
                results.push([target.a, target.b, Object.getPrototypeOf(target) === base].join(','));
                results.push(Object.getPrototypeOf({ __proto__: base }) === base);
                results.push(typeof { __proto__: null }.toString);
                results.push(Object.getPrototypeOf({ __proto__: null }) === null);
                results.push(Object.getOwnPropertyNames({ __proto__: base }).join(','));
                // Every other spelling of `__proto__` is an ordinary own
                // property: shorthand, method, accessor, and computed key.
                var __proto__ = 'own';
                results.push(Object.getOwnPropertyNames({ __proto__ }).join(','));
                results.push(Object.getOwnPropertyNames({ ['__proto__']: 1 }).join(','));
                results.push(Object.getOwnPropertyNames({ __proto__() {} }).join(','));
                results.push(Object.getOwnPropertyNames({ get __proto__() { return 1; } }).join(','));
                results.join(',');
            ",
        )
        .expect("__proto__ probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String(
            "true,false,true,true,true,true,true,true,1,2,true,true,undefined,true,,__proto__,__proto__,__proto__,__proto__"
                .to_owned()
        )
    );
}

#[test]
fn function_prototype_call_and_apply_thread_the_receiver() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                // The borrowed-base idiom: every one of these is how core-js,
                // jQuery-era helpers and the qq/bilibili bundles reuse a
                // prototype method on something that is not its receiver type.
                var owner = { a: 1, b: 2 };
                results.push(Object.prototype.hasOwnProperty.call(owner, 'a'));
                results.push(Object.prototype.hasOwnProperty.call(owner, 'zz'));
                results.push(Object.prototype.hasOwnProperty.call(Object(1), 'toString'));
                results.push(Object.prototype.hasOwnProperty.call('ab', '1'));
                results.push(Object.prototype.propertyIsEnumerable.call(owner, 'a'));
                results.push(String.prototype.indexOf.call('abcdef', 'cd'));
                results.push(String.prototype.toUpperCase.call('ab'));
                results.push(Number.prototype.toFixed.call(1.5, 1));
                results.push(Math.max.call(null, 1, 5, 2));
                results.push(Array.prototype.join.call({ length: 2, 0: 'p', 1: 'q' }, '-'));
                results.push(Array.prototype.map.call('abc', function (c) {
                    return c.toUpperCase();
                }).join(''));
                results.push(JSON.stringify.call(null, { a: 1 }));
                results.push(String.fromCharCode.call(null, 65, 66));
                // `thisArg` on every `callbackfn`: `map.call(list, render, self)`
                // is the whole point of a borrowed method.
                var box = { factor: 2, render: function (n) { return n * this.factor; } };
                results.push(Array.prototype.map.call([1, 2, 3], box.render, box).join(','));
                results.push(Array.prototype.filter.call([1, 2, 3, 4], box.isOdd || function (n) {
                    return n % this.factor === 1;
                }, box).join(','));
                var total = 0;
                Array.prototype.forEach.call([1, 2, 3], function (n) { total += n * this.factor; }, box);
                results.push(total);
                results.push(Array.prototype.some.call([1, 2, 3], function (n) {
                    return n * this.factor === 4;
                }, box));
                results.push(Array.prototype.every.call([2, 4], function (n) {
                    return n % this.factor === 0;
                }, box));
                results.push(Array.prototype.findIndex.call([1, 2, 3], function (n) {
                    return n * this.factor === 4;
                }, box));
                results.push(Array.from([1, 2], function (n) { return n * this.factor; }, box).join(','));
                results.push(new Uint8Array([1, 2]).map(function (n) {
                    return n * this.factor;
                }, box).join(','));
                // `.apply` with an array-like list, and `Reflect.apply` through
                // the same dispatch.
                results.push(Math.max.apply(null, [3, 9, 4]));
                results.push(Math.max.apply(null, { length: 3, 0: 5, 1: 11, 2: 7 }));
                results.push(String.prototype.indexOf.apply('abcdef', ['c']));
                results.push(Object.prototype.hasOwnProperty.apply(owner, ['a']));
                results.push(Reflect.apply(Math.max, null, [1, 5, 2]));
                results.push(Reflect.apply(function (x) { return this.base + x; }, { base: 40 }, [2]));
                results.push(Reflect.apply(Object.prototype.hasOwnProperty, owner, ['b']));
                // `.bind` pre-fills arguments and stays callable and constructable.
                results.push(String.prototype.indexOf.bind('abcde', 'c')());
                results.push(Object.prototype.hasOwnProperty.bind(owner)('a'));
                function Counter(value) { this.value = value; }
                results.push(new (Counter.bind(null, 8))().value);
                // `bind` is transparent about constructability: binding a
                // builtin with no [[Construct]] yields a function `new` still
                // rejects, so `new (Math.max.bind(null))` is a TypeError rather
                // than an `object`.
                results.push(typeof Math.max.bind(null));
                results.push((function () {
                    try { new (Math.max.bind(null)); return false; } catch (e) { return e instanceof TypeError; }
                })());
                results.join(',');
            ",
        )
        .expect("call/apply probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String(
            "true,false,false,true,true,2,AB,1.5,5,p-q,ABC,{\"a\":1},AB,2,4,6,1,3,12,\
             true,true,1,2,4,2,4,9,11,2,true,5,42,true,2,true,8,function,true"
                .to_owned()
        )
    );
}

#[test]
fn number_to_string_honours_a_radix_and_parse_int_reads_one() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                function thrown(fn) { try { fn(); return false; } catch (e) { return e instanceof RangeError; } }
                results.push((255).toString(16));
                results.push((255).toString(2));
                results.push((255).toString(36));
                results.push((-255).toString(16));
                results.push((0.5).toString(2));
                results.push((1.5).toString(2));
                results.push((0).toString(16));
                results.push((1e21).toString(10));
                results.push((0.1).toString(2).slice(0, 10));
                // Absent or undefined radix means 10; anything else out of range
                // is a RangeError.
                results.push((10).toString());
                results.push((10).toString(undefined));
                results.push((10).toString(2.9));
                results.push(thrown(function () { return (10).toString(1); }));
                results.push(thrown(function () { return (10).toString(37); }));
                results.push(thrown(function () { return (10).toString('x'); }));
                results.push((NaN).toString(16) + '/' + String(-Infinity).toString(16));
                // parseInt's radix argument, which used to be ignored.
                results.push(parseInt('ff', 16));
                results.push(parseInt('0x1f'));
                results.push(parseInt('0x1f', 16));
                results.push(parseInt('101', 2));
                results.push(parseInt('  -42px'));
                results.push(parseInt('zz', 36));
                results.push(parseInt('12', 1));
                results.push(String(parseInt('abc')));
                results.push(String(parseInt('0b11', 2)));
                results.push(parseInt('9999999999999999', 10));
                results.push(parseInt('7fffffff', 16));
                results.join(',');
            ",
        )
        .expect("radix probe executes");
    assert_eq!(
        outcome.value,
        // Every field is the value V8 produces for the probe above; the radix
        // cases in particular are `0.00011001` (0.1 is
        // 0.0001100110011... in binary), `1295` (z is digit 35, so 35*36+35),
        // `NaN` for an out-of-range radix, `0` for `parseInt('0b11', 2)`
        // (the `0b` prefix is only stripped for radix 16), and `1e16` because
        // 9999999999999999 is not representable as a double.
        JsValue::String(
            "ff,11111111,73,-ff,0.1,1.1,0,1e+21,0.00011001,10,10,1010,\
             true,true,true,NaN/-Infinity,255,31,31,5,-42,1295,NaN,NaN,0,\
             10000000000000000,2147483647"
                .to_owned()
        )
    );
}

#[test]
fn date_prototype_methods_see_a_primitive_receiver_as_nan() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                // `Date.prototype.getTime.call(5)` has no Date this-value, so
                // `thisTimeValue` answers NaN rather than a brand error.
                results.push(Date.prototype.getTime.call(5));
                results.push(Date.prototype.valueOf.call('x'));
                results.push(Date.prototype.getFullYear.call(5));
                results.push(Date.prototype.getTime.call(new Date(7)));
                results.push(Date.prototype.toString.call(5));
                // A genuine non-Date object is still a brand error.
                function thrown(fn) { try { fn(); return false; } catch (e) { return e instanceof TypeError; } }
                results.push(thrown(function () { return Date.prototype.getTime.call({}); }));
                results.join(',');
            ",
        )
        .expect("Date receiver probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String("NaN,NaN,NaN,7,Invalid Date,true".to_owned())
    );
}

#[test]
fn string_raw_reads_a_template_object_and_tags_name_their_hosts() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                function tag(value) { return Object.prototype.toString.call(value); }
                function thrown(fn) { try { fn(); return false; } catch (e) { return e instanceof TypeError; } }
                // A tagged template hands the tag a template object: an array
                // whose indices are the cooked strings and whose `raw` property
                // is the unprocessed text. The substitutions follow it, so
                // `String.raw` has something to splice between the literals.
                function highlight(strings) { return String.raw(strings, '<b>'.length, '</b>'); }
                results.push(highlight`a${0}b${1}c`);
                results.push(String.raw({ raw: ['x', 'y'] }, 'Q'));
                // A substitution is spliced only *between* literals, so a short
                // argument list appends nothing rather than stringifying a hole,
                // and an empty `raw` contributes nothing at all.
                results.push(String.raw({ raw: ['x', 'y'] }));
                results.push(String.raw({ raw: [] }));
                // An array-like without `raw` is a TypeError, not a string.
                results.push(thrown(function () {
                    return String.raw({ length: 2, 0: 'x', 1: 'y' });
                }));
                // The escape is resolved in the index and preserved in `raw`,
                // which is the whole difference between a template object and
                // the concatenated string a tag used to receive.
                results.push(String.raw`a\nb`);
                results.push(`a\nb`.length);
                var strings = null;
                (function (received) { strings = received; return ''; })`p${1}q${2}r`;
                results.push([strings.length, strings.raw.length, strings[0], strings[2]].join('|'));
                results.push(tag(strings));
                results.push([Array.isArray(strings), Array.isArray(strings.raw)].join(','));
                results.push(Object.getOwnPropertyDescriptor(strings, 'raw').enumerable);
                // §20.1.3.6: every host the engine models names itself. A
                // TypeError carries [[ErrorData]] like every other error, so it
                // tags as `Error`; `Object(null)` and `Object(undefined)` are
                // ordinary objects, so they tag as `Object`.
                results.push(tag(new Date(0)));
                results.push(tag(new Error('x')));
                results.push(tag(new TypeError('x')));
                results.push(tag(new Map()));
                results.push(tag(new Set()));
                results.push(tag(new Uint8Array(1)));
                results.push(tag(/re/));
                results.push(tag(Object(Symbol('s'))));
                results.push(tag(Object(1)));
                results.push(tag(Object('s')));
                results.push(tag(Object(true)));
                results.push(tag([]));
                results.push(tag(Object(null)));
                results.push(tag(Object(undefined)));
                results.push(tag(function () {}));
                results.push(tag(Math.max));
                results.push(tag(new Promise(function () {})));
                results.join(',');
            ",
        )
        .expect("String.raw probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String(
            "a3b</b>c,xQy,xy,,true,a\\nb,3,3|3|p|r,[object Array],true,true,false,\
[object Date],[object Error],[object Error],[object Map],\
[object Set],[object Uint8Array],[object RegExp],\
[object Symbol],[object Number],[object String],[object Boolean],\
[object Array],[object Object],[object Object],[object Function],\
[object Function],[object Promise]"
                .to_owned()
        )
    );
}

#[test]
fn a_boxed_wrapper_is_pinned_while_only_the_interpreter_frame_holds_it() {
    let parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    // White-box check of the GC root set: a fresh `ToObject` wrapper has no
    // script-visible slot, so the collector must treat the interpreter's own
    // reference as a root. Without the pin the slot is rewritten to a
    // prototype-less tombstone and the builtin keeps reading a dead object.
    let wrapper = runtime
        .to_object(&JsValue::String("ab".to_owned()))
        .expect("ToObject boxes a string");
    runtime.collect_garbage();
    let realm = runtime.realm();
    assert_eq!(
        realm.own_property_names(wrapper),
        Some(vec!["0".to_owned(), "1".to_owned(), "length".to_owned()])
    );
    let string_prototype = realm.global("String").and_then(|value| match value {
        JsValue::Object(constructor) => realm.get_property(constructor, "prototype"),
        _ => None,
    });
    assert_eq!(
        realm
            .object(wrapper)
            .and_then(crate::JsObject::prototype)
            .map(JsValue::Object),
        string_prototype
    );
    assert_eq!(
        realm.own_property(wrapper, "1").map(|entry| entry.value),
        Some(JsValue::String("b".to_owned()))
    );
}

#[test]
fn boxed_wrappers_survive_repeated_collections_during_a_builtin() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    // A heap this small forces `ensure_heap_capacity` to collect constantly,
    // so every `ToObject` wrapper the builtins create is pinned and released
    // thousands of times.
    let limits = crate::RuntimeLimits {
        max_heap_objects: 4_096,
        ..crate::RuntimeLimits::default()
    };
    let mut runtime = JsRuntime::with_limits(&parsed.dom, limits);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                var lost = -1;
                for (var i = 0; i < 20000; i++) {
                    var described = Object.getOwnPropertyDescriptors(Object('ab'));
                    if (Object.keys(described).join(',') !== '0,1,length') { lost = i; break; }
                    if (Object.getOwnPropertyNames(Object(1)).length !== 0) { lost = i; break; }
                }
                results.push(lost);
                var boxed = Object('ab');
                for (var j = 0; j < 20000; j++) {
                    var scratch = Object.getOwnPropertyDescriptor(5, 'toString');
                }
                results.push(Object.getPrototypeOf(boxed) === String.prototype);
                results.push(Object.getOwnPropertyNames(boxed).join(','));
                results.push(boxed[1]);
                results.join(',');
            ",
        )
        .expect("collection probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String("-1,true,0,1,length,b".to_owned())
    );
}

#[test]
fn string_prototype_symbol_iterator_yields_an_iterator_object() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                // `get-intrinsic` reads getProto(getProto('x'[Symbol.iterator]()))
                // to snapshot %IteratorPrototype%; an `undefined` answer there
                // silently empties the whole intrinsic table.
                results.push(typeof ''[Symbol.iterator] === 'function');
                results.push(''[Symbol.iterator] === ''.values);
                var iterator = 'abc'[Symbol.iterator]();
                results.push(typeof iterator === 'object' && typeof iterator.next === 'function');
                var first = iterator.next();
                results.push([first.value, first.done].join(','));
                var text = '';
                for (var character of 'xy') { text += character; }
                results.push(text);
                var spread = [...'ab'];
                results.push(spread.join('-'));
                var chain = Object.getPrototypeOf(Object.getPrototypeOf(iterator));
                results.push(chain === Object.prototype);
                results.join(',');
            ",
        )
        .expect("String iterator probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String("true,true,true,a,false,xy,a-b,true".to_owned())
    );
}

#[test]
fn web_storage_areas_answer_the_storage_interface() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var results = [];
                results.push(typeof localStorage.getItem === 'function');
                results.push(localStorage.length);
                results.push(String(localStorage.getItem('missing')));
                localStorage.setItem('theme', 'dark');
                localStorage.setItem('count', 7);
                results.push(localStorage.length);
                results.push(localStorage.getItem('theme'));
                results.push(localStorage.getItem('count'));
                results.push(localStorage.theme);
                results.push(Object.keys(localStorage).join(','));
                results.push(localStorage.key(0) + '/' + localStorage.key(1));
                results.push(localStorage.key(9));
                // The two areas are independent, and `clear` empties one.
                sessionStorage.setItem('only', 'here');
                results.push([localStorage.length, sessionStorage.length].join(','));
                localStorage.removeItem('theme');
                results.push([localStorage.getItem('theme'), localStorage.length].join(','));
                localStorage.clear();
                results.push([localStorage.length, sessionStorage.length].join(','));
                // A numeric slot is a stored entry; `length` is read-only.
                localStorage[0] = 'zero';
                results.push([localStorage[0], localStorage.length].join(','));
                localStorage.length = 99;
                results.push(localStorage.length);
                results.push(sessionStorage.getItem('only'));
                results.join(',');
            ",
        )
        .expect("Web Storage probe executes");
    assert_eq!(
        outcome.value,
        JsValue::String(
            "true,0,null,2,dark,7,dark,theme,count,theme/count,,2,1,,1,0,1,zero,1,1,here"
                .to_owned()
        )
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
fn blob_constructor_exposes_bounded_bytes_and_async_read_methods() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                var blob = new Blob(["hello", new Uint8Array([32, 119, 111, 114, 108, 100])], {type:"text/plain"});
                var summary = blob.size + ":" + blob.type;
                var objectUrl = URL.createObjectURL(blob);
                blob.slice(6).text().then(function (value) { summary += ":" + value; });
            "#,
        )
        .expect("Blob script should execute");
    drain_microtasks(&mut runtime, &mut parsed.dom);
    let outcome = runtime
        .execute(&mut parsed.dom, "summary")
        .expect("summary should be readable");
    assert_eq!(
        outcome.value,
        JsValue::String("11:text/plain:world".to_owned())
    );
    let url = runtime
        .execute(&mut parsed.dom, "objectUrl")
        .expect("object URL should be readable")
        .value
        .to_js_string();
    assert!(url.starts_with("data:text/plain;base64,"));
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
fn default_and_rest_parameters_bind_arguments_and_apply_defaults() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    function probe(a, b = 5, ...rest) {
                        return [a, b, typeof rest].join("|");
                    }
                    probe(1, 2, 3, 4) + " / " + probe(7) + " / " + probe(7, undefined);
                "#,
        )
        .expect("default/rest parameter call should execute");
    // Rest parameters collect the remaining arguments into an array; an
    // omitted or explicitly `undefined` argument evaluates the default.
    assert_eq!(
        outcome.value,
        JsValue::String("1|2|object / 7|5|object / 7|5|object".to_owned())
    );
}

#[test]
fn default_parameters_evaluate_when_arguments_are_undefined() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var calls = 0;
                    function add(a, b = a + 1, c = (calls += 1, 10)) {
                        return [a, b, c].join("|");
                    }
                    add(1) + " / " + add(1, 5, 7) + " / " + calls;
                "#,
        )
        .expect("default parameters should execute");
    // `b`'s initializer sees the earlier `a` binding; supplying `c`
    // suppresses its initializer, so `calls` only increments once.
    assert_eq!(
        outcome.value,
        JsValue::String("1|2|10 / 1|5|7 / 1".to_owned())
    );
}

#[test]
fn default_parameters_distinguish_undefined_from_null() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    function pick(value = "default") {
                        return value === null ? "null" : String(value);
                    }
                    pick(undefined) + " / " + pick(null) + " / " + pick() + " / " + pick(0);
                "#,
        )
        .expect("undefined-vs-null default probe should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("default / null / default / 0".to_owned())
    );
}

#[test]
fn throwing_default_parameters_propagate_before_the_body_runs() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var body = 0;
                    function explode(value = (function () { throw new Error("boom"); })()) {
                        body += 1;
                    }
                    var caught = "";
                    try { explode(); } catch (error) { caught = error.message; }
                    caught + " / " + body;
                "#,
        )
        .expect("throwing default parameter probe should execute");
    assert_eq!(outcome.value, JsValue::String("boom / 0".to_owned()));
}

#[test]
fn default_parameters_see_later_parameters_in_the_temporal_dead_zone() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var outer = "outer";
                    function probe(a = outer, b = later, later) {
                        return [a, b].join("|");
                    }
                    var caught = "";
                    try { probe(undefined, undefined, "third"); } catch (error) { caught = error.name; }
                    probe("first", "second", "third") + " / " + caught;
                "#,
        )
        .expect("temporal dead zone probe should execute");
    // A default reading a later parameter (or its own binding) must not fall
    // back to an outer binding of the same name.
    assert_eq!(
        outcome.value,
        JsValue::String("first|second / ReferenceError".to_owned())
    );
}

#[test]
fn object_and_class_methods_apply_parameter_defaults() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                    var object = {
                        double(value = 21) { return value * 2; },
                        arrow: (value = 3) => value + 1
                    };
                    class Counter {
                        constructor(step = 2) { this.step = step; }
                        advance(value = this.step) { return value + 1; }
                    }
                    object.double() + object.arrow() + new Counter().advance() + new Counter(10).advance();
                ",
        )
        .expect("method default parameter probe should execute");
    assert_eq!(outcome.value, JsValue::Number(60.0));
}

#[test]
fn parameter_defaults_do_not_change_function_length() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    function one(a, b = 1, c) {}
                    var two = function (x = 1, y) {};
                    var three = (...rest) => rest;
                    [one.length, two.length, three.length].join("|");
                "#,
        )
        .expect("function length probe should execute");
    // `length` counts parameters before the first default and excludes rest.
    assert_eq!(outcome.value, JsValue::String("1|0|0".to_owned()));
}

#[test]
fn destructuring_arrow_parameter_defaults_apply_to_the_whole_pattern() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var read = ({x} = {x: 7}) => x;
                    read() + "|" + read({x: 3});
                "#,
        )
        .expect("destructuring arrow default probe should execute");
    assert_eq!(outcome.value, JsValue::String("7|3".to_owned()));
}

/// Evaluate `source` and read one expression out of the resulting scope.
fn eval_read(source: &str, read: &str) -> String {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(&mut parsed.dom, source)
        .expect("destructuring should execute");
    runtime
        .execute(&mut parsed.dom, read)
        .expect("reading the destructured bindings should execute")
        .value
        .to_js_string()
}

#[test]
fn destructuring_declarations_bind_nested_defaults_rest_and_computed_keys() {
    assert_eq!(
        eval_read(
            r#"
                var [first, second = 9] = [1];
                var [, , third] = [1, 2, 3];
                var [head, ...tail] = [1, 2, 3];
                var {a: {b}, label = "none"} = {a: {b: 2}};
                var key = "computed";
                var {[key]: value, ...others} = {computed: 5, kept: 6};
            "#,
            r#"[first, second, third, head, tail.join("-"), b, label, value, others.kept].join("|")"#
        ),
        "1|9|3|1|2-3|2|none|5|6"
    );
}

#[test]
fn destructuring_declarations_bind_through_the_iterator_protocol() {
    // The point of this case is the *source* of the values, not the shape of
    // the bindings. Each source below yields values that indexed property
    // access would get wrong or not get at all.
    assert_eq!(
        eval_read(
            r#"
                var [a, b] = "xyz";
                var [entry] = new Map([[1, "one"]]);
                var [only] = new Set([7]);
                var [p, q] = new Uint8Array([1, 2]);
                var [first, ...rest] = new Uint8Array([4, 5, 6]);
                var source = {};
                source[Symbol.iterator] = function () {
                    var index = 0;
                    return {
                        next: function () {
                            return index < 3
                                ? { value: index++, done: false }
                                : { value: undefined, done: true };
                        }
                    };
                };
                var [m, n, o, missing] = source;
            "#,
            r#"[a, b, entry.join(":"), only, p, q, first, rest.join(","),
                m, n, o, missing === undefined, rest.length].join("|")"#
        ),
        "x|y|1:one|7|1|2|4|5,6|0|1|2|true|2"
    );
}

#[test]
fn a_destructured_string_walks_code_points_while_its_length_counts_code_units() {
    // The two answers are different on purpose: `length` is a UTF-16 count and
    // the pattern sees whole code points.
    assert_eq!(
        eval_read(
            "var [first, second] = '😀x';",
            "[first, second, second.length, first.length].join('|')"
        ),
        "😀|x|1|2"
    );
}

#[test]
fn a_destructuring_pattern_pulls_only_as_many_values_as_it_binds() {
    // Step counts are the observable difference between the iterator protocol
    // and draining the source into an array first. A rest element drains; a
    // pattern that finishes early does not, and closes the iterator instead.
    let counter = r"
        var steps = 0;
        var closed = 0;
        var source = {};
        source[Symbol.iterator] = function () {
            var index = 0;
            return {
                next: function () {
                    steps++;
                    return index < 5
                        ? { value: index++, done: false }
                        : { value: undefined, done: true };
                },
                return: function () { closed++; return {}; }
            };
        };
    ";
    assert_eq!(
        eval_read(
            &format!("{counter} var [one] = source;"),
            "steps + '/' + closed"
        ),
        // One value read, then closed because the source was not exhausted.
        "1/1"
    );
    assert_eq!(
        eval_read(
            &format!("{counter} var [one, two] = source;"),
            "steps + '/' + closed"
        ),
        "2/1"
    );
    assert_eq!(
        eval_read(
            &format!("{counter} var [, , three] = source;"),
            "steps + '/' + three"
        ),
        // An elision consumes a value rather than skipping the position.
        "3/2"
    );
    assert_eq!(
        eval_read(
            &format!("{counter} var [one, ...rest] = source;"),
            "steps + '/' + rest.length"
        ),
        // A rest element drains the source, so five values plus the final
        // `done` step, and nothing is left to close.
        "6/4"
    );
}

#[test]
fn a_destructuring_pattern_closes_the_iterator_when_a_binding_throws() {
    // The default for `b` runs because the source yields `undefined` there,
    // and it throws. Leaving the iterator open on an abrupt completion would
    // leak the source's resources, so the pattern must close it.
    assert_eq!(
        eval_read(
            r#"
                var steps = 0;
                var closed = 0;
                var source = {};
                source[Symbol.iterator] = function () {
                    var index = 0;
                    return {
                        next: function () {
                            steps++;
                            return index < 4
                                ? { value: [0, undefined, 2, 3][index++], done: false }
                                : { value: undefined, done: true };
                        },
                        return: function () { closed++; return {}; }
                    };
                };
                function boom() { throw new Error("late"); }
                var caught = "none";
                try {
                    var [a, b = boom()] = source;
                } catch (error) { caught = "threw"; }
            "#,
            "caught + '/' + steps + '/' + closed"
        ),
        "threw/2/1"
    );
}

#[test]
fn a_default_initializer_runs_only_for_an_undefined_value_and_only_once() {
    assert_eq!(
        eval_read(
            r"
                var present = 0;
                var missing = 0;
                var source = [1];
                var [a = present++] = source;
                var [b = missing++] = [undefined];
                var evaluations = 0;
                function make() { evaluations++; return [1, 2]; }
                var [x, y] = make();
            ",
            "[present, missing, evaluations, a, b, x, y].join('|')"
        ),
        "0|1|1|1|0|1|2"
    );
}

#[test]
fn an_object_pattern_reads_properties_in_order_and_excludes_them_from_the_rest() {
    assert_eq!(
        eval_read(
            r#"
                var order = [];
                var source = {
                    get a() { order.push("a"); return 1; },
                    get b() { order.push("b"); return 2; },
                    get c() { return 3; }
                };
                var {a, b, ...rest} = source;
            "#,
            "[order.join(''), rest.c, Object.keys(rest).join('')].join('|')"
        ),
        "ab|3|c"
    );
}

#[test]
fn destructuring_sources_that_cannot_iterate_throw_instead_of_binding_undefined() {
    // Every one of these has no `@@iterator`. Reading indexed properties off
    // them would quietly produce `undefined`, which is the failure this rules
    // out: a bundle that destructures a `Map` must not appear to work when the
    // engine really failed to find the iterator.
    for source in ["5", "undefined", "null", "true", "{0: 1, length: 1}"] {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let script = format!("var [a] = {source}; a;");
        let error = runtime
            .execute(&mut parsed.dom, &script)
            .expect_err("a non-iterable source must throw");
        assert_eq!(
            error.kind(),
            crate::JsErrorKind::Type,
            "`{source}` should raise a TypeError, got {error}"
        );
        assert!(
            error.message().contains("not iterable"),
            "`{source}` should say the source is not iterable, got: {}",
            error.message()
        );
    }
}

#[test]
fn an_object_pattern_refuses_a_nullish_source_while_a_primitive_boxes() {
    for source in ["undefined", "null"] {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let script = format!("var {{a}} = {source}; a;");
        let error = runtime
            .execute(&mut parsed.dom, &script)
            .expect_err("a nullish object pattern source must throw");
        assert_eq!(error.kind(), crate::JsErrorKind::Type, "got {error}");
    }
    // A primitive has no properties to read but is coercible, so it binds
    // `undefined` rather than throwing. This is the line between the two rules.
    assert_eq!(
        eval_read("var {missing} = 5;", "missing === undefined"),
        "true"
    );
    assert_eq!(eval_read("var {length} = 'abc';", "length"), "3");
}

#[test]
fn a_duplicate_lexical_binding_inside_a_pattern_is_an_early_error() {
    // `var` may reuse a name; `let` and `const` may not, and the check has to
    // see inside the pattern rather than only its declarator.
    for source in [
        "let [q, q] = [1, 2];",
        "const [q, q] = [1, 2];",
        "let {a: q, b: q} = {a: 1, b: 2};",
        "let {a: q, ...q} = {a: 1};",
        "let [q, {b: q}] = [1, {b: 2}];",
    ] {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let error = runtime
            .execute(&mut parsed.dom, source)
            .expect_err("a duplicate lexical binding must be rejected");
        assert_eq!(
            error.kind(),
            crate::JsErrorKind::Syntax,
            "{source}: {error}"
        );
    }
    // The same name under `var` is legal, and the last write wins.
    assert_eq!(eval_read("var [q, q] = [1, 2];", "q"), "2");
}

#[test]
fn a_malformed_destructuring_declaration_is_rejected_at_parse_time() {
    for source in [
        "var [a,,] = ;",
        "var [a",
        "var {a: } = {};",
        "var [...] = [1];",
        "var [a] =",
    ] {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime
            .execute(&mut parsed.dom, source)
            .expect_err("a malformed destructuring declaration must be rejected");
    }
}

#[test]
fn a_for_of_head_destructures_each_value_and_rebinds_lexical_names_per_iteration() {
    assert_eq!(
        eval_read(
            r#"
                var collected = "";
                for (const [key, value] of new Map([[1, 2]])) {
                    collected += key + ":" + value + ";";
                }
                var rows = [];
                for (let [value] of [[1], [2]]) {
                    rows.push(value);
                    value = 99;
                }
            "#,
            "[collected, rows.join(',')].join('|')"
        ),
        "1:2;|1,2"
    );
}

#[test]
fn destructuring_assignment_form_binds_members_and_iterator_sources() {
    assert_eq!(
        eval_read(
            r#"
                var first, second;
                [first, second] = [1, 2];
                var holder = {};
                [holder.value] = [9];
                ({x: holder.other} = {x: 8});
                var entry, tail;
                [entry, ...tail] = new Set(["only"]);
            "#,
            r#"[first, second, holder.value, holder.other, entry, tail.length].join("|")"#
        ),
        "1|2|9|8|only|0"
    );
}

/// Run a script, drain the microtask queue, run a follow-up script, drain
/// again. Combinator ordering needs the drain to happen *between* two
/// settlements, which is the whole point of the interleaving cases below.
fn settle_then_read(set_up: &str, after_first_drain: &str, read: &str) -> String {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(&mut parsed.dom, set_up)
        .expect("the combinator script should execute");
    drain_microtasks(&mut runtime, &mut parsed.dom);
    if !after_first_drain.is_empty() {
        runtime
            .execute(&mut parsed.dom, after_first_drain)
            .expect("the follow-up script should execute");
        drain_microtasks(&mut runtime, &mut parsed.dom);
    }
    runtime
        .execute(&mut parsed.dom, read)
        .expect("reading the combinator result should execute")
        .value
        .to_js_string()
}

#[test]
fn the_four_combinators_answer_the_empty_iterable_four_different_ways() {
    // These four answers are the usual bug: they collapse into "the empty case
    // is whatever the non-empty case does once the count reaches zero".
    assert_eq!(
        settle_then_read(
            "var out='pending'; Promise.all([]).then(function(v){out='fulfilled:'+JSON.stringify(v);},function(){out='rejected';});",
            "",
            "out",
        ),
        "fulfilled:[]"
    );
    assert_eq!(
        settle_then_read(
            "var out='pending'; Promise.allSettled([]).then(function(v){out='fulfilled:'+JSON.stringify(v);},function(){out='rejected';});",
            "",
            "out",
        ),
        "fulfilled:[]"
    );
    // Nothing can win a race that never starts, so this stays pending.
    assert_eq!(
        settle_then_read(
            "var out='pending'; Promise.race([]).then(function(){out='fulfilled';},function(){out='rejected';});",
            "",
            "out",
        ),
        "pending"
    );
    // Nothing succeeded, which is the one case that produces an `AggregateError`.
    assert_eq!(
        settle_then_read(
            r"
                var out='pending';
                Promise.any([]).then(function(){out='fulfilled';},function(e){
                    out = e.name + ':' + JSON.stringify(e.errors) + ':' + e.message;
                });
            ",
            "",
            "out",
        ),
        "AggregateError:[]:All promises were rejected"
    );
}

#[test]
fn promise_all_fulfils_with_every_value_in_iterable_order() {
    assert_eq!(
        settle_then_read(
            r"
                var out = 'pending';
                Promise.all([1, Promise.resolve(2), 'three']).then(function (values) {
                    out = JSON.stringify(values);
                }, function () { out = 'rejected'; });
            ",
            "",
            "out",
        ),
        r#"[1,2,"three"]"#
    );
    // A string is iterable and walks code points, so an astral character is one
    // element rather than two surrogate halves.
    assert_eq!(
        settle_then_read(
            "var out='pending'; Promise.all('a\u{1F600}b').then(function(v){out=JSON.stringify(v);});",
            "",
            "out",
        ),
        "[\"a\",\"\u{1F600}\",\"b\"]"
    );
}

#[test]
fn a_combinator_adopts_a_thenable_that_is_not_a_promise() {
    // The element has no `then` on a promise at all, so a combinator that only
    // understood native promises would bind the object itself.
    assert_eq!(
        settle_then_read(
            r"
                var out = 'pending';
                var thenable = { then: function (resolve) { resolve('adopted'); } };
                Promise.all([thenable]).then(function (v) { out = JSON.stringify(v); });
            ",
            "",
            "out",
        ),
        r#"["adopted"]"#
    );
    assert_eq!(
        settle_then_read(
            r"
                var out = 'pending';
                var thenable = { then: function (resolve) { resolve('adopted'); } };
                Promise.race([thenable]).then(function (v) { out = v; });
            ",
            "",
            "out",
        ),
        "adopted"
    );
}

#[test]
fn promise_all_rejects_with_the_first_reason_and_still_settles_the_rest() {
    // The two halves are what `all` is for: one rejection decides the outcome,
    // and no element is left pending behind it.
    assert_eq!(
        settle_then_read(
            r"
                var seen = [];
                Promise.all([
                    Promise.reject('first'),
                    Promise.reject('second')
                ]).catch(function (reason) { seen.push('caught:' + reason); });
                globalThis.settled = [];
                Promise.resolve('a').catch(function () {});
                Promise.reject('b').catch(function () {});
            ",
            "",
            "seen.join(',')",
        ),
        "caught:first"
    );
    assert_eq!(
        settle_then_read(
            r"
                var settled = [];
                var guarded = Promise.all([
                    Promise.reject('x'),
                    new Promise(function (_, reject) {
                        globalThis.finish = function () { reject('y'); };
                    })
                ]);
                guarded.catch(function () {});
                globalThis.report = function () {
                    return Promise.all([
                        Promise.reject('x'),
                        new Promise(function (resolve) { globalThis.finish2 = resolve; })
                    ]).catch(function () { return 'caught'; });
                };
                globalThis.count = function () { return settled.length; };
            ",
            r"
                var t = 0;
                Promise.resolve().then(function(){ t++; });
                Promise.resolve().then(function(){ t++; });
            ",
            "String(t)",
        ),
        "2"
    );
}

#[test]
fn promise_all_settled_never_rejects_and_reports_each_element_by_index() {
    assert_eq!(
        settle_then_read(
            r"
                var out = 'pending';
                Promise.allSettled([
                    Promise.resolve(1),
                    Promise.reject('nope')
                ]).then(function (settled) {
                    out = JSON.stringify(settled)
                        + '|keys:' + Object.keys(settled[0]).join('+')
                        + '|' + Object.keys(settled[1]).join('+');
                }, function () { out = 'rejected'; });
            ",
            "",
            "out",
        ),
        r#"[{"status":"fulfilled","value":1},{"status":"rejected","reason":"nope"}]|keys:status+value|status+reason"#
    );
}

#[test]
fn promise_any_aggregates_every_reason_into_errors_in_index_order() {
    assert_eq!(
        settle_then_read(
            r"
                var out = 'pending';
                Promise.any([
                    Promise.reject('a'),
                    Promise.reject('b'),
                    Promise.reject('c')
                ]).then(function () { out = 'fulfilled'; }, function (error) {
                    out = JSON.stringify(error.errors) + '|' + error.name + '|' + error.message;
                });
            ",
            "",
            "out",
        ),
        r#"["a","b","c"]|AggregateError|All promises were rejected"#
    );
    // Index order, not arrival order: the second element rejects first, but it
    // still has to appear second in `errors`.
    assert_eq!(
        settle_then_read(
            r"
                var out = 'pending';
                var finishFirst;
                var first = { then: function (_, reject) { finishFirst = function () { reject('first'); }; } };
                Promise.any([first, Promise.reject('second')])
                    .catch(function (error) { out = JSON.stringify(error.errors); });
                globalThis.releaseFirst = function () { finishFirst(); };
            ",
            "releaseFirst();",
            "out",
        ),
        r#"["first","second"]"#
    );
    // `any` fulfils on the first success and ignores the later failure.
    assert_eq!(
        settle_then_read(
            r"
                var out = 'pending';
                Promise.any([Promise.resolve('ok'), Promise.reject('late')])
                    .then(function (v) { out = v; }, function () { out = 'rejected'; });
            ",
            "",
            "out",
        ),
        "ok"
    );
}

#[test]
fn promise_race_settles_once_and_the_first_settlement_wins() {
    assert_eq!(
        settle_then_read(
            r"
                var out = 'pending';
                var late;
                var second = new Promise(function (resolve) { late = function () { resolve('second'); }; });
                Promise.race([Promise.resolve('first'), second])
                    .then(function (v) { out = v; });
                globalThis.releaseLate = late;
            ",
            "releaseLate();",
            "out",
        ),
        "first"
    );
    // The first *rejection* settles a race just as firmly as a fulfilment.
    assert_eq!(
        settle_then_read(
            r"
                var out = 'pending';
                Promise.race([Promise.reject('rejected first'), Promise.resolve('never')])
                    .then(function () { out = 'fulfilled'; }, function (e) { out = 'rejected:' + e; });
            ",
            "",
            "out",
        ),
        "rejected:rejected first"
    );
}

#[test]
fn a_combinator_rejects_a_source_that_cannot_iterate_instead_of_throwing() {
    // The call itself returns a promise; the failure arrives through it. That is
    // what makes `Promise.all(responseLike).catch(...)` a working pattern.
    for (source, expected) in [
        ("Promise.all(5)", "TypeError"),
        ("Promise.allSettled(5)", "TypeError"),
        ("Promise.any(5)", "TypeError"),
        ("Promise.race(5)", "TypeError"),
        ("Promise.all(undefined)", "TypeError"),
        ("Promise.all({0: 1, length: 1})", "TypeError"),
    ] {
        assert_eq!(
            settle_then_read(
                &format!("var out='pending'; {source}.catch(function(e){{out=e.name;}});"),
                "",
                "out",
            ),
            expected,
            "{source} should reject with a TypeError"
        );
    }
}

#[test]
fn a_throwing_iterator_is_a_distinct_path_from_an_empty_settlement() {
    // `any` over an iterable that throws rejects with *that* error, not with an
    // `AggregateError`. Conflating the two is the specific bug this rules out:
    // the two produce different errors and call sites handle them differently.
    for combinator in ["all", "allSettled", "any", "race"] {
        assert_eq!(
            settle_then_read(
                &format!(
                    r"
                        var out = 'pending';
                        var bad = {{}};
                        bad[Symbol.iterator] = function () {{
                            return {{ next: function () {{ throw new Error('iterator threw'); }} }};
                        }};
                        Promise.{combinator}(bad).then(function () {{
                            out = 'fulfilled';
                        }}, function (error) {{
                            out = error.message + ':' + (error.errors ? 'has errors' : 'no errors');
                        }});
                    ",
                ),
                "",
                "out",
            ),
            "iterator threw:no errors",
            "Promise.{combinator} over a throwing iterator"
        );
    }
}

#[test]
fn aggregate_error_carries_its_reasons_as_an_own_non_enumerable_property() {
    assert_eq!(
        eval_read(
            r#"
                var error = new AggregateError([1, 2], "why");
            "#,
            r#"[error.errors.join(","), error.message, error.name, error.toString()].join("|")"#
        ),
        "1,2|why|AggregateError|AggregateError: why"
    );
    assert_eq!(
        eval_read(
            "var error = new AggregateError([1], 'm');",
            r#"[error instanceof Error, error instanceof AggregateError,
                Object.prototype.hasOwnProperty.call(error, 'errors'),
                Object.keys(error).length].join("|")"#
        ),
        "true|true|true|0"
    );
    // `errors` is writable and non-enumerable, and `JSON.stringify` sees `{}`.
    assert_eq!(
        eval_read(
            "var error = new AggregateError([1], 'm'); error.errors = [9];",
            r#"[error.errors.join(","), JSON.stringify(error),
                Object.getOwnPropertyDescriptor(error, 'errors').enumerable].join("|")"#
        ),
        "9|{}|false"
    );
    // `errors` is required to be iterable, so the constructor throws without it,
    // and the prototype carries the ordinary error `name`/`message` pair.
    assert_eq!(
        eval_read(
            r"
                var caught = 'none';
                try { new AggregateError(); } catch (error) { caught = error.name; }
            ",
            "[caught, AggregateError.length, AggregateError.name, AggregateError.prototype.name].join('|')"
        ),
        "TypeError|2|AggregateError|AggregateError"
    );
}

#[test]
fn the_combinators_are_constructor_statics_with_a_length_of_one() {
    assert_eq!(
        eval_read(
            r#"
                var names = ["all", "allSettled", "any", "race"];
            "#,
            r#"[Promise.all.length, Promise.allSettled.length, Promise.any.length,
                Promise.race.length, typeof Promise.prototype.all,
                Object.getOwnPropertyDescriptor(Promise, "any").enumerable].join("|")"#
        ),
        "1|1|1|1|undefined|false"
    );
}

#[test]
fn arrow_default_parameters_see_the_lexical_this() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    var holder = {
                        base: 10,
                        read: function () {
                            var arrow = (value = this.base) => value;
                            return arrow() + "|" + arrow(4);
                        }
                    };
                    holder.read();
                "#,
        )
        .expect("arrow default `this` probe should execute");
    assert_eq!(outcome.value, JsValue::String("10|4".to_owned()));
}

#[test]
fn derived_constructor_parameters_apply_defaults_before_super() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                    class Base {
                        constructor(value) { this.value = value; }
                    }
                    class Derived extends Base {
                        constructor(value = 5) { super(value * 2); }
                    }
                    new Derived().value + "|" + new Derived(3).value;
                "#,
        )
        .expect("derived constructor default probe should execute");
    assert_eq!(outcome.value, JsValue::String("10|6".to_owned()));
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
    assert_eq!(error.kind(), crate::JsErrorKind::Reference);
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
    assert_eq!(error.kind(), crate::JsErrorKind::Throw);
    let outcome = runtime
        .execute(&mut parsed.dom, "window.failures.join(\";\");")
        .expect("failure probe should execute");
    assert_eq!(
        outcome.value,
        JsValue::String("microtask boom:typed".to_owned())
    );
}

#[test]
fn document_cookie_round_trips_and_deletes() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                document.cookie = 'a=1';
                document.cookie = 'b=hello world';
                var read1 = document.cookie;
                document.cookie = 'a=; expires=Thu, 01 Jan 1970 00:00:00 GMT';
                var read2 = document.cookie;
                read1 + ' | ' + read2;
            ",
        )
        .expect("cookie round trip executes");
    assert_eq!(
        outcome.value,
        JsValue::String("a=1; b=hello world | b=hello world".to_owned())
    );
}

#[test]
fn class_declarations_build_constructors_prototypes_and_methods() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                class Point {
                    constructor(x, y) { this.x = x; this.y = y; }
                    sum() { return this.x + this.y; }
                    get doubled() { return this.sum() * 2; }
                    set scalar(value) { this.x = value; this.y = value; }
                    static origin() { return new Point(0, 0); }
                }
                var p = new Point(2, 3);
                var o = Point.origin();
                p.sum() === 5 && p.doubled === 10 &&
                    (p.scalar = 4) === 4 && p.x === 4 && p.y === 4 &&
                    o.x === 0 && o.y === 0 &&
                    Point.prototype.constructor === Point &&
                    Object.getPrototypeOf(p) === Point.prototype &&
                    Point.name === "Point" &&
                    p instanceof Point;
            "#,
        )
        .expect("class declaration executes");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn class_inheritance_super_fields_and_private_names_work() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                class Base {
                    #secret = 1;
                    constructor(name) { this.name = name; }
                    describe() { return "base:" + this.name; }
                    reveal() { return this.#secret; }
                }
                class Derived extends Base {
                    #secret = 2;
                    label = "L";
                    static kind = "derived";
                    constructor(name) { super(name + "!"); }
                    describe() { return "derived:" + super.describe(); }
                    parentSecret() { return this.#secret; }
                    hasSecret(object) { return #secret in object; }
                }
                var d = new Derived("x");
                d.name === "x!" && d.describe() === "derived:base:x!" &&
                    d.label === "L" && Derived.kind === "derived" &&
                    d.reveal() === 1 && d.parentSecret() === 2 &&
                    d instanceof Derived && d instanceof Base &&
                    Object.getPrototypeOf(Derived) === Base &&
                    d.hasSecret(d) && !d.hasSecret({});
            "#,
        )
        .expect("class inheritance executes");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn class_expressions_and_default_derived_constructor_work() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                var Named = class Inner {
                    static label() { return Inner.name; }
                };
                class Base {
                    constructor(value) { this.value = value; }
                }
                class Derived extends Base {}
                var d = new Derived(7);
                Named.name === "Inner" && Named.label() === "Inner" &&
                    d.value === 7 && d.constructor === Derived;
            "#,
        )
        .expect("class expression executes");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn iterator_helpers_map_filter_take_and_terminals() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                var mapped = [1, 2, 3].values().map(function (x) { return x * 2; }).toArray().join(",");
                var filtered = [1, 2, 3, 4].values().filter(function (x) { return x % 2 === 0; }).toArray().join(",");
                var taken = [1, 2, 3].values().take(2).toArray().join(",");
                var dropped = [1, 2, 3].values().drop(1).toArray().join(",");
                var reduced = [1, 2, 3].values().reduce(function (a, b) { return a + b; }, 10);
                var appended = [1, 2].values().concat([3, 4].values()).toArray().join(",");
                var found = [1, 5, 3].values().find(function (x) { return x > 4; });
                var some = [1, 2].values().some(function (x) { return x === 2; });
                var every = [1, 2].values().every(function (x) { return x > 0; });
                var chunks = [1, 2, 3, 4, 5].values().chunks(2).toArray().map(function (c) { return c.join("+"); }).join("|");
                var windows = [1, 2, 3].values().windows(2).toArray().map(function (c) { return c.join("-"); }).join("|");
                var flat = [1, 2].values().flatMap(function (x) { return [x, x * 10]; }).toArray().join(",");
                var fromIterable = Iterator.from([7, 8]).toArray().join(",");
                mapped + "|" + filtered + "|" + taken + "|" + dropped + "|" + reduced + "|" +
                    appended + "|" + found + "|" + some + "|" + every + "|" + chunks + "|" +
                    windows + "|" + flat + "|" + fromIterable;
            "#,
        )
        .expect("iterator helpers execute");
    assert_eq!(
        outcome.value,
        JsValue::String(
            "2,4,6|2,4|1,2|2,3|16|1,2,3,4|5|true|true|1+2|3+4|5|1-2|2-3|1,10,2,20|7,8".to_owned()
        )
    );
}

#[test]
fn classes_can_extend_iterator_and_use_helpers() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                class Countdown extends Iterator {
                    constructor(start) { super(); this.current = start; }
                    next() {
                        if (this.current < 0) { return { done: true }; }
                        return { value: this.current--, done: false };
                    }
                }
                var viaHelper = new Countdown(3).map(function (x) { return x + 1; }).toArray().join(",");
                var selfIterable = [1].values()[Symbol.iterator]() === [1].values()[Symbol.iterator]();
                var direct = (function () {
                    var iterator = [9].values();
                    return iterator[Symbol.iterator]() === iterator;
                })();
                viaHelper + "|" + selfIterable + "|" + direct;
            "#,
        )
        .expect("Iterator subclass executes");
    assert_eq!(
        outcome.value,
        JsValue::String("4,3,2,1|false|true".to_owned())
    );
}

#[test]
fn nested_new_inside_constructor_body_keeps_its_own_new_target() {
    // babel-transpiled classes (bilibili main bundle): a constructor body that
    // runs `new Helper(...)` must construct the Helper with `Helper` itself as
    // new.target. The engine used to leak the enclosing constructor's
    // new.target into nested `new` expressions, so the Helper instance received
    // the OUTER class's prototype and the inner `_classCallCheck`
    // (`this instanceof Helper`) threw "Cannot call a class as a function".
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                function _classCallCheck(instance, ctor) {
                    if (!(instance instanceof ctor)) {
                        throw new TypeError("Cannot call a class as a function");
                    }
                }
                var Helper = (function () {
                    function Helper() { _classCallCheck(this, Helper); }
                    return Helper;
                })();
                var Outer = (function () {
                    function Outer() {
                        _classCallCheck(this, Outer);
                        this.helper = new Helper();
                    }
                    Outer.prototype.spawn = function () { return new Outer(); };
                    return Outer;
                })();
                var outer = new Outer();
                outer.helper instanceof Helper &&
                    !(outer.helper instanceof Outer) &&
                    (outer.spawn().helper instanceof Helper);
            "#,
        )
        .expect("nested construction inside a constructor body should execute");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn nested_new_sees_its_own_new_target_inside_class_constructors() {
    // The instance created by a nested `new` links to the nested constructor's
    // `prototype`, never to the enclosing constructor's new.target prototype,
    // and `new.target` inside the nested body is the nested constructor.
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var seen = [];
                var inner;
                var Inner = function Inner() { seen.push(new.target === Inner); };
                class Outer {
                    constructor() { inner = new Inner(); }
                }
                new Outer();
                seen.length === 1 && seen[0] === true &&
                    Object.getPrototypeOf(inner) === Inner.prototype &&
                    inner instanceof Inner && !(inner instanceof Outer);
            ",
        )
        .expect("new.target inside nested new should be the nested constructor");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn direct_iterator_new_stays_abstract_after_new_target_scope_fix() {
    // `new Iterator()` is abstract even when it appears inside another
    // constructor body; only `super()` from an Iterator subclass constructs.
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let error = runtime
        .execute(
            &mut parsed.dom,
            r"
                class Wrap {
                    constructor() { this.iterator = new Iterator(); }
                }
                new Wrap();
            ",
        )
        .expect_err("direct new Iterator() inside a constructor body is abstract");
    assert_eq!(error.kind(), crate::JsErrorKind::Type);
}

#[test]
fn iterator_subclass_super_still_constructs_through_new_target_scope_fix() {
    // The abstract-Iterator exemption must keep working for `super()` from a
    // subclass after new.target scoping is tightened for nested `new`.
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                class Countdown extends Iterator {
                    constructor(start) { super(); this.current = start; }
                    next() {
                        if (this.current < 0) { return { done: true }; }
                        return { value: this.current--, done: false };
                    }
                }
                new Countdown(2).map(function (x) { return x * 2; }).toArray().join(",");
            "#,
        )
        .expect("Iterator subclass super() should keep constructing");
    assert_eq!(outcome.value, JsValue::String("4,2,0".to_owned()));
}

#[test]
fn object_define_property_keeps_symbol_keys_as_symbols() {
    // core-js (bilibili log-reporter): installs `Symbol.unscopables` on
    // `Array.prototype` via `Object.defineProperty`, then module code reads
    // `proto[Symbol.unscopables]["keys"] = true`. The engine used to coerce
    // the symbol key to the string "Symbol(unscopables)", so the later
    // symbol-keyed read returned undefined and threw
    // "Cannot read properties of undefined (reading 'keys')".
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
                var proto = Array.prototype;
                var key = Symbol.unscopables;
                var symbolsBefore = Object.getOwnPropertySymbols(proto).length;
                void 0 === proto[key] &&
                    Object.defineProperty(proto, key, { configurable: true, value: {} });
                var slot = proto[key];
                slot["keys"] = true;
                slot.keys === true &&
                    Object.getOwnPropertySymbols(proto).length === symbolsBefore + 1 &&
                    Object.hasOwn(proto, key) &&
                    Object.getOwnPropertyDescriptor(proto, key).configurable === true &&
                    typeof proto["Symbol(unscopables)"] === "undefined";
            "#,
        )
        .expect("symbol-keyed defineProperty roundtrip should execute");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn symbol_define_property_rejects_invalid_redefinitions() {
    // A non-configurable symbol property rejects value changes but accepts
    // the identical no-op redefinition, mirroring string-keyed semantics.
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            r#"
                globalThis.__object = {};
                globalThis.__key = Symbol("tag");
                Object.defineProperty(__object, __key, {
                    value: 1,
                    writable: false,
                    configurable: false,
                });
            "#,
        )
        .expect("initial definition succeeds");
    let error = runtime
        .execute(
            &mut parsed.dom,
            "Object.defineProperty(__object, __key, { value: 2 });",
        )
        .expect_err("changing a non-configurable symbol property must throw");
    assert_eq!(error.kind(), crate::JsErrorKind::Type);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                Object.defineProperty(__object, __key, { value: 1, writable: false, configurable: false });
                var symbols = Object.getOwnPropertySymbols(__object);
                __object[__key] === 1 && symbols.length === 1;
            ",
        )
        .expect("no-op redefinition and probe run");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn repeated_gc_preserves_live_count_and_prototype_walks() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            "globalThis.savedPrototype = {}; globalThis.savedChild = Object.create(savedPrototype);",
        )
        .expect("set up reachable objects");
    runtime.collect_garbage();
    let first_live_count = runtime.realm.object_count();
    runtime.collect_garbage();
    assert_eq!(runtime.realm.object_count(), first_live_count);
    let outcome = runtime
        .execute(&mut parsed.dom, "savedPrototype.isPrototypeOf(savedChild)")
        .expect("prototype chain remains usable after repeated collection");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn object_set_prototype_of_changes_lookup_and_rejects_cycles() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r"
                var base = { value: 7 };
                var child = {};
                Object.setPrototypeOf(child, base);
                var lookup = child.value === 7 && base.isPrototypeOf(child);
                var cycleRejected = false;
                try { Object.setPrototypeOf(base, child); } catch (_) { cycleRejected = true; }
                Object.preventExtensions(child);
                var noop = Object.setPrototypeOf(child, base) === child;
                lookup && cycleRejected && noop;
            ",
        )
        .expect("Object.setPrototypeOf should update the real prototype chain");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn global_var_does_not_replace_read_only_window_parent() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            "var parent = 42; parent === window && window.parent === window && top === window",
        )
        .expect("global var may coexist with a read-only Window property");
    assert_eq!(outcome.value, JsValue::Boolean(true));
}

#[test]
fn document_create_comment_produces_a_comment_node() {
    let mut parsed = parse_document("<!doctype html><p></p>");
    let url = Url::parse("https://comment.test/").expect("test URL");
    let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
    let outcome = runtime
        .execute(
            &mut parsed.dom,
            r#"
            const comment = document.createComment("feature-detect");
            comment.nodeType + ":" + comment.nodeValue
        "#,
        )
        .expect("createComment runs");
    assert_eq!(
        outcome.value,
        JsValue::String("8:feature-detect".to_owned())
    );
}
