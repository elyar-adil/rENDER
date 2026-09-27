//! Offline probe for the qq.com main bundle that killed the page.
//!
//! `.diag/qq/GAP_REPORT.md` section 1 records the blocking failure: the
//! 2.27 MB production bundle
//! `mat1.gtimg.com/qqcdn/qqindex2021/common-static/pc-home/.../index.js`
//! aborted with `TypeError: getProto: not an object at line 206, column 825`
//! inside the bundled `get-proto` shim, which called
//! `Object.getPrototypeOf` on a primitive. That one function decided whether
//! the hero carousel and every JS-populated section appeared.
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p render-js --example qq_bundle_probe -- [bundle.js] [--html FILE] [--no-shim]
//! ```
//!
//! Defaults to `../../.diag/qq/index.js` and a synthetic document that carries
//! the bundle's `qqhome-*` mount points. The real page's React/ReactDOM bundles
//! are not checked in, so the probe installs a permissive host shim for them
//! (see [`REACT_SHIM`]); the shim never throws, which keeps the reported
//! failure an engine-semantics gap rather than a stub artifact. `--no-shim`
//! reports the failure with no host globals at all.
//!
//! Reports per run:
//!
//! - `compile`: whether the bundle even parses;
//! - `first failure`: kind, message, byte offset, and line/column, with the
//!   source text around the offset so the minified expression is readable;
//! - `progress`: interpreter steps consumed, console messages emitted, and
//!   element count, which move measurably as engine gaps are closed.
//!
//! Diagnostic flags:
//!
//! - `--module-log` rewrites the bundle's webpack require so a throwing module
//!   logs its registry id (the engine's frame labels carry byte offsets, not
//!   module ids);
//! - `RENDER_QQ_TRACE_PROTO=1` logs every nullish `Reflect.getPrototypeOf`
//!   call with a JS stack;
//! - `--pre-reflect` deletes the `Reflect` prototype builtins so the run
//!   reproduces the pre-fix `getProto: not an object` failure for comparison.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use render_html::parse_document;
use render_js::{JsError, JsRuntime, RuntimeLimits};
use url::Url;

/// Synthetic document with the mount points the bundle's bootstrap renders
/// into. The saved artifacts contain the CSS and JS but not the served HTML,
/// so this stands in for it; the probe measures JavaScript semantics, not
/// layout.
const DEFAULT_HTML: &str = r#"<!doctype html><html><head><title>qq</title></head><body>
<div id="qqhome-hot-picks"></div>
<div id="qqhome-hot-video"></div>
<div id="qqhome-footer"></div>
<div id="qqhome-channel-feed"></div>
<div id="qqhome-float-btn"></div>
<div id="qqhome-swiper"></div>
<div id="qqhome-side-bar"></div>
<div id="qqhome-top-wrap"></div>
<div id="qqhome-news-list"></div>
<div id="app"></div>
</body></html>"#;

/// Permissive React/ReactDOM stand-ins for the two UMD globals the bundle
/// reads off `globalThis` (`require("React")`, `require("ReactDOM")`). Every
/// hook is inert and `createElement` returns a plain descriptor object, so
/// nothing here can throw: whatever the probe reports next is the engine's.
const REACT_SHIM: &str = r#"
(function () {
  var noop = function () { return null; };
  function element(type, props, children) {
    var rest = Array.prototype.slice.call(arguments, 2);
    return {
      $$typeof: Symbol.for("react.element"),
      type: type,
      key: props && props.key != null ? props.key : null,
      ref: props && props.ref != null ? props.ref : null,
      props: Object.assign({}, props || {}, { children: rest.length === 1 ? rest[0] : rest })
    };
  }
  function context(defaultValue) {
    var record = { _currentValue: defaultValue, _defaultValue: defaultValue };
    return {
      Provider: function (props) { return props && props.value; },
      Consumer: function () { return noop; },
      _context: record
    };
  }
  function isValidElement(value) {
    return !!value && value.$$typeof === Symbol.for("react.element");
  }
  function flatten(children, out) {
    out = out || [];
    if (children == null || children === false) return out;
    if (Array.isArray(children)) {
      for (var i = 0; i < children.length; i++) flatten(children[i], out);
      return out;
    }
    out.push(children);
    return out;
  }
  var React = {
    __isProbeShim: true,
    version: "18.3.1-probe-shim",
    Fragment: Symbol.for("react.fragment"),
    StrictMode: Symbol.for("react.strict_mode"),
    Profiler: Symbol.for("react.profiler"),
    Suspense: Symbol.for("react.suspense"),
    createElement: element,
    cloneElement: function (existing) { return element(existing.type, existing.props); },
    isValidElement: isValidElement,
    createElementFactory: function () { return element; },
    createRef: function () { return { current: null }; },
    createContext: context,
    createFactory: function () { return element; },
    forwardRef: function (render) { return render; },
    memo: function (component) { return component; },
    lazy: function (load) { return { then: function () {} }; },
    Children: {
      map: function (children, fn) { return flatten(children).map(fn); },
      forEach: function (children, fn) { flatten(children).forEach(fn); },
      count: function (children) { return flatten(children).length; },
      toArray: function (children) { return flatten(children); },
      only: function (children) { return flatten(children)[0]; }
    },
    Component: function Component() {},
    PureComponent: function PureComponent() {},
    useState: function (initial) { return [initial, noop]; },
    useReducer: function (reducer, initial) { return [initial, noop]; },
    useEffect: noop,
    useLayoutEffect: noop,
    useInsertionEffect: noop,
    useImperativeHandle: noop,
    useMemo: function (factory) { return factory(); },
    useCallback: function (fn) { return fn; },
    useRef: function (initial) { return { current: initial }; },
    useContext: function (ctx) { return ctx && ctx._currentValue; },
    useDebugValue: noop,
    useId: function () { return ":r0:"; },
    useSyncExternalStore: function (subscribe, getSnapshot) { return getSnapshot(); },
    useTransition: function () { return [false, noop]; },
    useDeferredValue: function (value) { return value; },
    startTransition: noop
  };
  React.Component.prototype.isReactComponent = {};
  React.PureComponent.prototype.isPureReactComponent = true;
  React.__SECRET_INTERNALS_DO_NOT_USE_OR_YOU_WILL_BE_FIRED = {
    ReactCurrentDispatcher: { current: null },
    ReactCurrentOwner: { current: null },
    ReactCurrentBatchConfig: { transition: null },
    ReactDebugCurrentFrame: {},
    assign: Object.assign
  };
  globalThis.React = React;
  globalThis.ReactDOM = {
    __isProbeShim: true,
    version: React.version,
    render: function () { return null; },
    hydrate: function () { return null; },
    createRoot: function () {
      return { render: noop, unmount: noop };
    },
    hydrateRoot: function () {
      return { render: noop, unmount: noop };
    },
    unmountComponentAtNode: function () { return true; },
    createPortal: function (children) { return children; },
    findDOMNode: function () { return null; },
    flushSync: function (fn) { return fn ? fn() : undefined; },
    unstable_batchedUpdates: function (fn, argument) { return fn(argument); },
    unstable_renderSubtreeIntoContainer: function () { return null; }
  };
  globalThis.__REACT_DEVTOOLS_GLOBAL_HOOK__ = {
    renderers: new Map(),
    supportsFiber: true,
    inject: function () {},
    onCommitFiberRoot: function () {},
    onCommitFiberUnmount: function () {}
  };
  // Feature-detection probes the bundle performs before the first render.
  globalThis.requestIdleCallback =
    globalThis.requestIdleCallback || function (fn) { return setTimeout(fn, 0); };
  globalThis.cancelIdleCallback =
    globalThis.cancelIdleCallback || function (id) { clearTimeout(id); };
  globalThis.requestAnimationFrame =
    globalThis.requestAnimationFrame || function (fn) { return setTimeout(fn, 16); };
  globalThis.cancelAnimationFrame =
    globalThis.cancelAnimationFrame || function (id) { clearTimeout(id); };
  if (typeof globalThis.IntersectionObserver !== "function") {
    globalThis.IntersectionObserver = function () {
      this.observe = function () {};
      this.unobserve = function () {};
      this.disconnect = function () {};
      this.takeRecords = function () { return []; };
    };
  }
  if (typeof globalThis.ResizeObserver !== "function") {
    globalThis.ResizeObserver = function () {
      this.observe = function () {};
      this.unobserve = function () {};
      this.disconnect = function () {};
    };
  }
})();
"#;

/// Opt-in tracing, enabled with `RENDER_QQ_TRACE_PROTO=1`. The bundle picks
/// `getProto` from `get-proto@1.0.1`, which prefers `Reflect.getPrototypeOf`
/// and falls back to a stricter `Object.getPrototypeOf` wrapper. Which branch
/// runs, and what the argument is, decides whether a nullish argument is a
/// bundle bug or an engine gap, so log every nullish call with a JS stack.
const PROTO_TRACE_SHIM: &str = r#"
(function () {
  var original = Reflect.getPrototypeOf;
  Reflect.getPrototypeOf = function (target) {
    if (target === null || target === undefined) {
      console.error(
        "[probe] Reflect.getPrototypeOf(" + String(target) + ")\n" +
        new Error("trace").stack
      );
    }
    return original.call(Reflect, target);
  };
})();
"#;

/// Restores the engine surface this session started from, so the "before"
/// number in the report is measured on this machine rather than quoted.
///
/// The three JS-semantics gaps the bundle walks into, in the order it walks
/// into them:
///
/// 1. `Reflect.getPrototypeOf` was not installed at all. `get-proto@1.0.1`
///    picks its implementation with
///    `"undefined" != typeof Reflect && Reflect.getPrototypeOf || null` at
///    module init; with the property absent it falls through to a stricter
///    `Object.getPrototypeOf` wrapper that throws for non-objects. That is the
///    `TypeError: getProto: not an object` recorded in
///    `.diag/qq/browser.log` line 136.
/// 2. `Object.getPrototypeOf` itself answered every non-object primitive with
///    a `TypeError` instead of boxing it into a wrapper.
/// 3. `String.prototype[Symbol.iterator]` and `Object.prototype.__proto__`
///    were missing, which `get-intrinsic` and the Vue/Helux reactivity layer
///    need respectively.
///
/// `localStorage` cannot be removed (its global binding is non-configurable)
/// and is only reached far past the first failure, so it does not affect the
/// comparison. The failing position this mode reports is byte-identical to the
/// one the pre-session browser run recorded.
const PRE_REFLECT_SHIM: &str = r#"
(function () {
  delete Reflect.getPrototypeOf;
  delete Reflect.setPrototypeOf;
  delete Reflect.apply;
  delete Reflect.isExtensible;
  delete Reflect.preventExtensions;
  delete String.prototype[Symbol.iterator];
  delete Object.prototype.__proto__;
  // The pre-fix `Object.getPrototypeOf` answered a nullish argument with
  // `null` and rejected every non-object primitive.
  var wrapper = Object.getPrototypeOf;
  Object.getPrototypeOf = function (value) {
    if (value === null || value === undefined) {
      return null;
    }
    if (typeof value !== "object" && typeof value !== "function") {
      throw new TypeError("primitive object coercion is not implemented in this runtime slice");
    }
    return wrapper(value);
  };
})();
"#;

fn main() -> ExitCode {
    // Minified production bundles recurse deeply; mirror the browser shell and
    // run everything on a dedicated big-stack thread.
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(probe_main)
        .expect("spawn probe thread");
    match handle.join() {
        Ok(code) => code,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[derive(Default)]
struct Options {
    bundle: Option<PathBuf>,
    html: Option<PathBuf>,
    shim: bool,
    module_log: bool,
    pre_reflect: bool,
}

fn parse_options() -> Options {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.diag/qq");
    let mut options = Options {
        shim: true,
        ..Options::default()
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--no-shim" => options.shim = false,
            "--module-log" => options.module_log = true,
            "--pre-reflect" => options.pre_reflect = true,
            "--html" => options.html = arguments.next().map(PathBuf::from),
            other if other.starts_with("--") => {
                eprintln!("unknown option {other}");
            }
            other => options.bundle = Some(PathBuf::from(other)),
        }
    }
    if options.bundle.is_none() {
        options.bundle = Some(repo_root.join("index.js"));
    }
    options
}

fn probe_main() -> ExitCode {
    let options = parse_options();
    let bundle_path = options.bundle.expect("bundle path");
    let Ok(original) = fs::read_to_string(&bundle_path) else {
        eprintln!("cannot read {}", bundle_path.display());
        return ExitCode::FAILURE;
    };
    let html = match &options.html {
        Some(path) => fs::read_to_string(path).unwrap_or_else(|_| DEFAULT_HTML.to_owned()),
        None => DEFAULT_HTML.to_owned(),
    };
    println!(
        "bundle: {} ({} bytes)",
        bundle_path.display(),
        original.len()
    );
    let source = if options.module_log {
        let Some(instrumented) = instrument_webpack_require(&original) else {
            eprintln!("--module-log: webpack require shape not recognised");
            return ExitCode::FAILURE;
        };
        instrumented
    } else {
        original.clone()
    };
    let patch_delta = source.len().saturating_sub(original.len());

    let mut parsed = parse_document(&html);
    let dom = &mut parsed.dom;
    let base = Url::parse("https://www.qq.com/").expect("base URL");
    // The bundle is a single script turn that must run to completion, so raise
    // the per-script step budget; `max_execution_steps` is a runaway guard, not
    // a budget a real page can hit.
    let limits = RuntimeLimits {
        max_execution_steps: 4_000_000_000,
        max_heap_objects: 1_048_576,
        ..RuntimeLimits::default()
    };
    let mut runtime = JsRuntime::with_limits_and_url(dom, limits, &base);

    if options.shim {
        if let Err(error) = runtime.execute(dom, REACT_SHIM) {
            eprintln!("host shim failed: {error}");
            return ExitCode::FAILURE;
        }
        if std::env::var_os("RENDER_QQ_TRACE_PROTO").is_some() {
            if let Err(error) = runtime.execute(dom, PROTO_TRACE_SHIM) {
                eprintln!("proto trace shim failed: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    if options.pre_reflect {
        if let Err(error) = runtime.execute(dom, PRE_REFLECT_SHIM) {
            eprintln!("pre-reflect shim failed: {error}");
            return ExitCode::FAILURE;
        }
    }

    let started = Instant::now();
    let outcome = runtime.execute(dom, &source);
    let elapsed = started.elapsed();
    let steps = runtime.steps_consumed();
    let console = runtime.take_console_messages();

    println!(
        "elements after run: {} | console messages: {} | steps: {steps} | wall: {:.2}s",
        count_elements(dom, dom.document()),
        console.len(),
        elapsed.as_secs_f64()
    );
    for message in console.iter().take(5) {
        let text: String = message.text.chars().take(240).collect();
        println!("  [console.{}] {text}", message.level.label());
    }
    if console.len() > 5 {
        println!("  … {} more console messages", console.len() - 5);
    }

    match outcome {
        Ok(script) => {
            println!(
                "RESULT: bundle completed (completion value: {})",
                script.value.to_js_string()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            report_failure(&error, &source, patch_delta);
            println!(
                "RESULT: FAILED | steps: {steps} | wall: {:.2}s",
                elapsed.as_secs_f64()
            );
            ExitCode::FAILURE
        }
    }
}

/// The single expression every webpack bundle uses to run a module factory,
/// in the minified form this bundle ships. Wrapping the factory call is the
/// only reliable way to learn *which* module of 1500 threw: the engine's frame
/// labels carry byte offsets, not module ids, and the bundle's own map is a
/// private local.
const WEBPACK_REQUIRE_CALL: &str =
    "return a[e].call(r.exports,r,r.exports,u),r.loaded=!0,r.exports";
const WEBPACK_REQUIRE_CALL_PATCH: &str = "try{return a[e].call(r.exports,r,r.exports,u),r.loaded=!0,r.exports}catch(e){try{console.error(\"[probe] module threw: \"+r.id+\"\\n\"+(e&&e.stack||e))}catch(t){}throw e}";

/// Rewrite the bundle so a throwing module logs its registry id. Returns
/// `None` when the shape is not recognised, so the probe never silently
/// instruments nothing.
fn instrument_webpack_require(source: &str) -> Option<String> {
    source
        .contains(WEBPACK_REQUIRE_CALL)
        .then(|| source.replacen(WEBPACK_REQUIRE_CALL, WEBPACK_REQUIRE_CALL_PATCH, 1))
}

/// Print the first failure with everything needed to locate it in the 2.27 MB
/// minified source: the engine's own line/column, the byte offset, and the
/// expression around it. `patch_delta` is the number of bytes `--module-log`
/// inserted before the failure, so the offset is also reported against the
/// checked-in bundle.
fn report_failure(error: &JsError, source: &str, patch_delta: usize) {
    println!("FIRST FAILURE: kind={:?}", error.kind());
    println!("  message: {}", error.message());
    if let Some((line, column)) = error.position() {
        println!("  position: line {line}, column {column}");
    } else {
        println!("  position: unresolved");
    }
    let Some(offset) = error.offset() else {
        println!("  byte offset: none (host-thrown value)");
        return;
    };
    let original = offset.saturating_sub(patch_delta);
    if patch_delta == 0 {
        println!("  byte offset: {offset} of {}", source.len());
    } else {
        println!(
            "  byte offset: {offset} of {} (patched; {original} in the checked-in bundle)",
            source.len()
        );
    }
    let start = text_floor_boundary(source, offset.saturating_sub(220));
    let end = text_ceil_boundary(source, (offset + 220).min(source.len()));
    println!("  source[{start}..{end}]:");
    println!("    {}", source[start..end].replace('\n', "\\n"));
    println!("    {}^", " ".repeat(offset - start));
}

/// A byte offset that is a UTF-8 character boundary at or below `offset`.
fn text_floor_boundary(source: &str, offset: usize) -> usize {
    let mut index = offset.min(source.len());
    while index > 0 && !source.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn text_ceil_boundary(source: &str, offset: usize) -> usize {
    let mut index = offset.min(source.len());
    while index < source.len() && !source.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Number of element nodes in the tree, a coarse "how much did the bundle
/// actually build" signal when the run dies before its first render lands.
fn count_elements(dom: &render_dom::Dom, node: render_dom::NodeId) -> usize {
    let Some(reference) = dom.node(node) else {
        return 0;
    };
    let self_count = usize::from(matches!(reference.kind(), render_dom::NodeKind::Element(_)));
    self_count
        + dom
            .children(node)
            .unwrap_or_default()
            .iter()
            .map(|child| count_elements(dom, *child))
            .sum::<usize>()
}
