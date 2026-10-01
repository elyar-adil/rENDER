//! Classify a WPT test: what does it assert, and what does it need to run?
//!
//! This module produces the *denominator* for every number the runner reports.
//! A conformance percentage is only interpretable against a classification of
//! what was in the suite and why each test did or did not produce evidence, and
//! that classification is what this file is.
//!
//! The classifier is conservative in one specific direction. It will call a
//! test [`Feasibility::Blocked`] if it finds a known missing capability, and it
//! will call a test [`Feasibility::Unknown`] if it could not scan the file
//! confidently. It will call a test [`Feasibility::Executable`] only if it found
//! *no* known blocker. It does not claim a test is runnable merely because it
//! failed to find a reason not to run it, and absence of evidence of a blocker
//! is reported as `unknown`, never as `executable`. A denominator built by
//! optimistic classification is how a harness ends up quietly scoring the
//! tests it happened to support.

use crate::source::Scan;

/// The four testharness.js shapes this runner can express, plus the
/// `testcss.js` entry points and the legacy `setup()` harness.
///
/// A shape outside this set is a skip with
/// [`SkipReason::UnsupportedHarnessShape`](crate::outcome::SkipReason), never a
/// failure. WPT added shapes over the years; an unknown one is a gap in this
/// file, and reporting it as a gap is the whole point of naming it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HarnessShape {
    /// `test(fn, name)` - synchronous, assertions thrown as exceptions.
    SynchronousTest,
    /// `promise_test(fn, name)` - returns a promise.
    PromiseTest,
    /// `async_test(fn)` - done-callback based.
    AsyncTest,
    /// `assert_throws_js(ctor, fn)` or `assert_throws_js(fn)`.
    AssertThrowsJs,
    /// `setup(fn)` - the pre-2014 harness. Still present in the suite.
    LegacySetup,
    /// `test_parsed_value` / `test_computed_value` / `test_invalid_value` /
    /// `test_valid_value` and the `test_*_rule` family, i.e. `testcss.js`.
    TestCss,
    /// `link rel="match"` - compared by rendered reference, not by assertion.
    Reftest,
}

impl HarnessShape {
    pub const fn label(self) -> &'static str {
        match self {
            Self::SynchronousTest => "test",
            Self::PromiseTest => "promise_test",
            Self::AsyncTest => "async_test",
            Self::AssertThrowsJs => "assert_throws_js",
            Self::LegacySetup => "setup",
            Self::TestCss => "testcss.js",
            Self::Reftest => "reftest",
        }
    }

    /// Whether this runner implements the shape.
    ///
    /// # `LegacySetup` is supported, and that was a bug
    ///
    /// The last round recorded `setup()` as an unsupported harness shape and
    /// blocked 772 tests on it. That was wrong, and wrong in the direction that
    /// hides work: `testharness.js` at the pinned revision does implement it -
    /// `expose(setup, 'setup')` at line 1294, and the function runs
    /// synchronously before the tests via `tests.setup(func, properties)`. The
    /// runner does not reimplement `setup`; it *loads WPT's harness*, which
    /// means `setup` works for the same reason `test` works.
    ///
    /// The general rule this encodes: a shape is unsupported when **this
    /// runner** cannot express it, never when the shape looks old. Deciding by
    /// age would have blocked `setup`, and would next block `async_test` for
    /// being older still, and the executable population would shrink with every
    /// release of the suite rather than with every release of this runner.
    pub const fn is_supported(self) -> bool {
        matches!(
            self,
            Self::SynchronousTest
                | Self::PromiseTest
                | Self::AsyncTest
                | Self::AssertThrowsJs
                | Self::LegacySetup
                | Self::TestCss
                | Self::Reftest
        )
    }
}

/// A host capability a test requires before its assertions mean anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// A nested browsing context. WPT's own cross-frame test protocol and all of
    /// `testcss.js` are built on it.
    NestedBrowsingContext,
    /// `parent` / `top` window access - how a WPT test reports a result to the
    /// driver when it runs in a frame.
    WindowProxy,
    /// A subresource loaded by URL, which needs a document base and a loader.
    ExternalResource,
    /// `fetch` / `XMLHttpRequest`.
    NetworkFetch,
    /// `WebSocket`.
    WebSocket,
    /// `Worker` / `SharedWorker`.
    DedicatedWorker,
    /// `localStorage` / `sessionStorage` / `indexedDB`.
    ClientStorage,
    /// Canvas 2D or WebGL.
    Canvas,
    /// `AudioContext` and friends.
    WebAudio,
    /// `navigator.gpu`.
    WebGpu,
    /// `RTCPeerConnection`.
    WebRtc,
    /// `WebAssembly`.
    WebAssembly,
    /// `Range` and selection.
    Range,
    /// `attachShadow` / `ShadowRoot`.
    ShadowDom,
    /// `requestAnimationFrame` / `Element.animate`.
    Animation,
    /// `HTMLMediaElement` playback.
    MediaPlayback,
    /// Timer functions. WPT's `async_test` depends on these.
    Timers,
    /// Manual `dispatchEvent` and event listeners driving a test.
    EventDispatch,
    /// Cross-document navigation or `history` manipulation.
    Navigation,
    /// A permissions, geolocation, clipboard, speech or payments API.
    DeviceApis,
    /// Geolocation.
    Geolocation,
    /// Dynamic `import()`.
    DynamicImport,
}

impl Capability {
    pub const fn label(self) -> &'static str {
        match self {
            Self::NestedBrowsingContext => "nested-browsing-context",
            Self::WindowProxy => "window-proxy",
            Self::ExternalResource => "external-resource",
            Self::NetworkFetch => "network-fetch",
            Self::WebSocket => "websocket",
            Self::DedicatedWorker => "dedicated-worker",
            Self::ClientStorage => "client-storage",
            Self::Canvas => "canvas",
            Self::WebAudio => "webaudio",
            Self::WebGpu => "webgpu",
            Self::WebRtc => "webrtc",
            Self::WebAssembly => "wasm",
            Self::Range => "range",
            Self::ShadowDom => "shadow-dom",
            Self::Animation => "animation",
            Self::MediaPlayback => "media-playback",
            Self::Timers => "timers",
            Self::EventDispatch => "event-dispatch",
            Self::Navigation => "navigation",
            Self::DeviceApis => "device-apis",
            Self::Geolocation => "geolocation",
            Self::DynamicImport => "dynamic-import",
        }
    }

    /// Every capability, for iterating capability coverage in a report.
    pub const ALL: &'static [Self] = &[
        Self::NestedBrowsingContext,
        Self::WindowProxy,
        Self::ExternalResource,
        Self::NetworkFetch,
        Self::WebSocket,
        Self::DedicatedWorker,
        Self::ClientStorage,
        Self::Canvas,
        Self::WebAudio,
        Self::WebGpu,
        Self::WebRtc,
        Self::WebAssembly,
        Self::Range,
        Self::ShadowDom,
        Self::Animation,
        Self::MediaPlayback,
        Self::Timers,
        Self::EventDispatch,
        Self::Navigation,
        Self::DeviceApis,
        Self::Geolocation,
        Self::DynamicImport,
    ];
}

/// Why a test cannot produce evidence about the engine, in the order of how
/// much it matters to a reader trying to act on the number.
/// The strongest blocker found.
///
/// Not `Copy`: two of the variants carry a message, and the message is the
/// part a person reads when a test silently drops out of the denominator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocker {
    /// The engine does not implement a capability the test needs. A feature
    /// gap, and a legitimate reason to be at zero on that area.
    MissingEngine(Capability),
    /// The test uses an assertion shape this runner does not implement. A gap
    /// in the runner, and never an engine defect.
    UnsupportedShape(HarnessShape),
    /// A declared fixture is absent from the checkout.
    MissingFixture(String),
    /// The file could not be scanned confidently, so nothing can be claimed
    /// about it.
    Unscannable(String),
    /// The test declares no assertion and no reference. It has no way to fail,
    /// so scoring it would inflate the pass count. This is the same defect that
    /// inflated a test count in this project.
    NoAssertions,
}

impl Blocker {
    pub fn describe(&self) -> String {
        match self {
            Self::MissingEngine(capability) => {
                format!("engine lacks {}", capability.label())
            }
            Self::UnsupportedShape(shape) => {
                format!("this runner does not implement the {} harness shape", shape.label())
            }
            Self::MissingFixture(url) => format!("declared fixture is absent: {url}"),
            Self::Unscannable(why) => format!("could not scan confidently: {why}"),
            Self::NoAssertions => "declares no assertion and no reftest reference".to_owned(),
        }
    }
}

/// The strongest blocker found. Ordering is by `Blocker`'s declaration order, so
/// a test blocked by both a missing engine capability and an unsupported shape
/// is attributed to the engine, which is the harder truth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classification {
    /// Harness shapes detected, sorted, deduplicated.
    pub shapes: Vec<HarnessShape>,
    /// Capabilities the test appears to need, sorted, deduplicated.
    pub capabilities: Vec<Capability>,
    /// Number of assertion *call sites* in inline script.
    ///
    /// A call-site count, not a judgement: it cannot tell an assertion on a
    /// real engine-produced value from one on a value the harness computed
    /// itself. The execution tier records what was actually evaluated; this is
    /// the cheap static bound that makes a zero visible.
    pub assertion_sites: usize,
    /// The blocker that decided feasibility.
    pub blocker: Option<Blocker>,
}

impl Classification {
    /// Whether this test could produce evidence about the engine, as far as
    /// static analysis can tell.
    pub const fn is_executable(&self) -> bool {
        self.blocker.is_none()
    }

    /// Whether this test has no way to fail, and so must not be scored.
    ///
    /// Distinct from "blocked": a blocked test is one this runner declined to
    /// run, whereas this is a property of the *test*, which would be true in any
    /// browser. A crash-guard test that only checks the page did not crash is a
    /// real WPT idiom, and counting those as passes is exactly the inflation
    /// this project already suffered once.
    pub const fn cannot_fail(&self) -> bool {
        matches!(self.blocker, Some(Blocker::NoAssertions))
    }
}

/// Classify one scanned test file.
///
/// `test_dir` and `suite_root` locate declared fixtures in the checkout. Both
/// are `None` when the caller has no filesystem, in which case fixtures are
/// assumed present rather than guessed absent: guessing absent here would drop
/// real tests out of the population, which is the expensive direction to be
/// wrong in.
pub fn classify(
    scan: &Scan,
    inline_scripts: &[&str],
    test_dir: Option<&std::path::Path>,
    suite_root: Option<&std::path::Path>,
) -> Classification {
    let mut shapes: Vec<HarnessShape> = Vec::new();
    let mut assertion_sites = 0usize;
    let joined = inline_scripts.join("\n");

    // Assertion call sites, counted by family. `assert_*` and the `test_*`
    // families are what actually decide a WPT test; the `test*` entry points
    // are shapes.
    for name in [
        "assert_equals",
        "assert_not_equals",
        "assert_true",
        "assert_false",
        "assert_array_equals",
        "assert_object_equals",
        "assert_approx_equals",
        "assert_unreached",
        "assert_class_string",
        "assert_own_property",
        "assert_in_array",
        "assert_less_than",
        "assert_greater_than",
        "assert_regexp_match",
    ] {
        assertion_sites += count_calls(&joined, name);
    }
    // `assert_throws_js` is both a shape and an assertion site.
    let throws_sites = count_calls(&joined, "assert_throws_js");
    if throws_sites > 0 {
        shapes.push(HarnessShape::AssertThrowsJs);
        assertion_sites += throws_sites;
    }

    let sync = count_calls(&joined, "test");
    if sync > 0 {
        shapes.push(HarnessShape::SynchronousTest);
        assertion_sites += sync;
    }
    let promise = count_calls(&joined, "promise_test");
    if promise > 0 {
        shapes.push(HarnessShape::PromiseTest);
        assertion_sites += promise;
    }
    let async_ = count_calls(&joined, "async_test");
    if async_ > 0 {
        shapes.push(HarnessShape::AsyncTest);
        assertion_sites += async_;
    }
    let legacy = count_calls(&joined, "setup");
    if legacy > 0 {
        shapes.push(HarnessShape::LegacySetup);
    }

    // testcss.js entry points. Any of them means the test drives an iframe.
    let mut testcss = 0usize;
    for name in [
        "test_parsed_value",
        "test_computed_value",
        "test_invalid_value",
        "test_valid_value",
        "test_invalid_argument",
        "test_property_value",
        "test_rule",
        "test_selector",
        "test_declaration",
        "test_serialize_value",
        "test_css_value",
        "test_strict_parsing",
        "test_at_rule",
        "test_style_rule",
        "test_font_feature_values",
    ] {
        let n = count_calls(&joined, name);
        testcss += n;
        assertion_sites += n;
    }
    if testcss > 0 {
        shapes.push(HarnessShape::TestCss);
    }
    if scan.reftest_reference.is_some() {
        shapes.push(HarnessShape::Reftest);
    }

    shapes.sort_unstable();
    shapes.dedup();

    let mut capabilities = detect_capabilities(scan, &joined, testcss > 0);

    // Declared fixtures that are not in the checkout. A test whose support file
    // is missing would be scored against the wrong stylesheet, which is a fail
    // for the wrong reason and one of the most expensive ways to waste a run.
    let mut missing_fixture = None;
    if let Some(dir) = test_dir {
        for resource in &scan.resources {
            if let Some(resolved) = resolve_fixture(dir, suite_root, &resource.url) {
                if !resolved.exists() {
                    missing_fixture = Some(resource.url.clone());
                    break;
                }
            }
        }
    }
    if let Some(url) = missing_fixture {
        capabilities.retain(|c| *c != Capability::ExternalResource);
        return Classification {
            shapes,
            capabilities,
            assertion_sites,
            blocker: Some(Blocker::MissingFixture(url)),
        };
    }

    if let Some(why) = &scan.uncertain {
        return Classification {
            shapes,
            capabilities,
            assertion_sites,
            blocker: Some(Blocker::Unscannable(why.clone())),
        };
    }

    let blocker = pick_blocker(&shapes, &capabilities, assertion_sites);
    Classification { shapes, capabilities, assertion_sites, blocker }
}

/// Decide the single blocker that governs feasibility.
fn pick_blocker(
    shapes: &[HarnessShape],
    capabilities: &[Capability],
    assertion_sites: usize,
) -> Option<Blocker> {
    // A test with no assertion site and no reftest has nothing to fail. This is
    // checked first: it is a property of the test itself, and reporting it as
    // "blocked" would misattribute it to the engine.
    if assertion_sites == 0 && !shapes.contains(&HarnessShape::Reftest) {
        return Some(Blocker::NoAssertions);
    }
    for shape in shapes {
        if !shape.is_supported() {
            return Some(Blocker::UnsupportedShape(*shape));
        }
    }
    // Then capabilities, in `Capability::ALL` order, so the reported blocker is
    // stable across runs and across machines.
    for capability in Capability::ALL {
        if capabilities.contains(capability) && !capability_is_available(*capability) {
            return Some(Blocker::MissingEngine(*capability));
        }
    }
    None
}

/// What this engine exposes to a test, as a *static* statement about the
/// adapter's known surface.
///
/// This is a claim about a known list, and it is written out explicitly rather
/// than derived, because a derived answer would move silently as the engine
/// changes and every recorded number would quietly change meaning. The
/// authoritative version of this table is the one in
/// `tools/wpt/FINDINGS.md`, written against a specific engine revision.
pub const fn capability_is_available(capability: Capability) -> bool {
    match capability {
        // Timers, event dispatch and external resources have engine support and
        // the adapter can express them.
        Capability::Timers | Capability::EventDispatch | Capability::ExternalResource => true,
        // render-core's own registry records that nested browsing contexts are
        // not implemented: "an `iframe` renders as nothing". Every `testcss.js`
        // entry point needs one, so the entire CSS area is blocked here.
        Capability::NestedBrowsingContext | Capability::WindowProxy => false,
        Capability::NetworkFetch
        | Capability::WebSocket
        | Capability::DedicatedWorker
        | Capability::ClientStorage
        | Capability::Canvas
        | Capability::WebAudio
        | Capability::WebGpu
        | Capability::WebRtc
        | Capability::WebAssembly
        | Capability::Range
        | Capability::ShadowDom
        | Capability::Animation
        | Capability::MediaPlayback
        | Capability::Navigation
        | Capability::DeviceApis
        | Capability::Geolocation
        | Capability::DynamicImport => false,
    }
}

fn detect_capabilities(scan: &Scan, script: &str, uses_testcss_entry_points: bool) -> Vec<Capability> {
    let mut found = Vec::new();
    let push = |cap: Capability, found: &mut Vec<Capability>| {
        if !found.contains(&cap) {
            found.push(cap);
        }
    };

    // WPT's own `testcss.js` is an iframe driver. Three independent signals say
    // so, and all three are honoured, because any one of them can be absent:
    //
    //  * the file is named in a `<script src>`;
    //  * the source mentions the file;
    //  * the test calls one of the entry points, possibly through indirection
    //    that defeats textual matching but not a call site.
    //
    // The third matters most: a test that calls `test_parsed_value` needs an
    // iframe even if it never says the word.
    let declares_testcss = scan
        .external_scripts
        .iter()
        .chain(scan.resources.iter().map(|r| &r.url))
        .any(|url| url.contains("testcss.js"));
    if declares_testcss || mentions(script, "testcss.js") || uses_testcss_entry_points {
        push(Capability::NestedBrowsingContext, &mut found);
        push(Capability::ExternalResource, &mut found);
    }

    if mentions(script, "iframe") || script.contains("createElement(\"iframe\"") {
        push(Capability::NestedBrowsingContext, &mut found);
    }
    // `parent` and `top` are window references in a WPT test, whether written
    // bare (`parent.postMessage`) or qualified (`window.parent`).
    //
    // A false positive here blocks a test that might otherwise have run, which
    // is the safe direction: it costs a skip that is then explained, rather than
    // claiming a test is runnable and discovering otherwise at scoring time.
    for name in ["parent", "top"] {
        if mentions_property(script, name) || identifier_before_dot(script, name) {
            push(Capability::WindowProxy, &mut found);
        }
    }
    if !scan.resources.is_empty() {
        push(Capability::ExternalResource, &mut found);
    }
    if mentions_calls(script, "fetch") || mentions_calls(script, "XMLHttpRequest") {
        push(Capability::NetworkFetch, &mut found);
    }
    if mentions_calls(script, "WebSocket") {
        push(Capability::WebSocket, &mut found);
    }
    if mentions_calls(script, "Worker") {
        push(Capability::DedicatedWorker, &mut found);
    }
    if mentions_property(script, "localStorage")
        || mentions_property(script, "sessionStorage")
        || mentions_property(script, "indexedDB")
    {
        push(Capability::ClientStorage, &mut found);
    }
    if mentions_calls(script, "getContext") || mentions_calls(script, "CanvasRenderingContext2D") {
        push(Capability::Canvas, &mut found);
    }
    if mentions_calls(script, "AudioContext") || mentions_calls(script, "OfflineAudioContext") {
        push(Capability::WebAudio, &mut found);
    }
    if mentions_property(script, "gpu") {
        push(Capability::WebGpu, &mut found);
    }
    if mentions_calls(script, "RTCPeerConnection") {
        push(Capability::WebRtc, &mut found);
    }
    if mentions_calls(script, "WebAssembly") {
        push(Capability::WebAssembly, &mut found);
    }
    if mentions_calls(script, "createRange") {
        push(Capability::Range, &mut found);
    }
    if mentions_calls(script, "attachShadow") {
        push(Capability::ShadowDom, &mut found);
    }
    if mentions_calls(script, "requestAnimationFrame") || mentions_calls(script, "animate") {
        push(Capability::Animation, &mut found);
    }
    if mentions_calls(script, "play") && mentions_property(script, "currentTime") {
        push(Capability::MediaPlayback, &mut found);
    }
    if mentions_calls(script, "setTimeout")
        || mentions_calls(script, "setInterval")
        || mentions_calls(script, "clearTimeout")
        || mentions_calls(script, "clearInterval")
    {
        push(Capability::Timers, &mut found);
    }
    if mentions_calls(script, "dispatchEvent") {
        push(Capability::EventDispatch, &mut found);
    }
    if mentions_property(script, "location") || mentions_property(script, "history") {
        push(Capability::Navigation, &mut found);
    }
    if mentions_calls(script, "getPermissions")
        || mentions_calls(script, "clipboard")
        || mentions_calls(script, "SpeechRecognition")
        || mentions_calls(script, "PaymentRequest")
    {
        push(Capability::DeviceApis, &mut found);
    }
    if mentions_calls(script, "getCurrentPosition") {
        push(Capability::Geolocation, &mut found);
    }
    if script.contains("import(") {
        push(Capability::DynamicImport, &mut found);
    }

    found.sort_unstable();
    found
}

/// Resolve a WPT URL against the suite layout.
///
/// WPT serves two bases, and picking the wrong one produces a *confidently
/// wrong* "missing fixture" verdict - which then excludes a real test from the
/// population over a path that was never missing:
///
/// * a rooted URL (`/css/reference/square.html`) is suite-root-relative;
/// * a relative URL (`support/a.css`) is relative to the test's directory.
///
/// Returns `None` for a URL that is not a fetchable suite path - an `http(s)`
/// URL, or one of the pseudo-URLs WPT uses (`about:blank`, `data:`, `blob:`,
/// `javascript:`, `filesystem:`). Those are not missing fixtures, and treating
/// them as absent would drop real DOM tests out of the population.
fn resolve_fixture(
    test_dir: &std::path::Path,
    suite_root: Option<&std::path::Path>,
    url: &str,
) -> Option<std::path::PathBuf> {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    if url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("//")
        || url.starts_with("about:")
        || url.starts_with("data:")
        || url.starts_with("blob:")
        || url.starts_with("javascript:")
        || url.starts_with("filesystem:")
        || url.is_empty()
    {
        return None;
    }
    if let Some(rooted) = url.strip_prefix('/') {
        // Rooted: only resolvable when the suite root is known.
        let root = suite_root?;
        return Some(root.join(rooted));
    }
    // Reject traversal out of the checkout rather than following it.
    if url.split('/').any(|seg| seg == "..") {
        return None;
    }
    Some(test_dir.join(url))
}

/// Count call sites of a bare identifier call, excluding method calls
/// (`obj.test(`) and property definitions (`function test(`).
fn count_calls(source: &str, name: &str) -> usize {
    let bytes = source.as_bytes();
    let mut count = 0usize;
    let mut i = 0usize;
    let name_bytes = name.as_bytes();
    while i + name_bytes.len() <= bytes.len() {
        if &bytes[i..i + name_bytes.len()] != name_bytes {
            i += 1;
            continue;
        }
        // `obj.test(` is a method call, not a bare call to `test`.
        let method_call = i > 0 && bytes[i - 1] == b'.';
        let before_ok = (i == 0 || !is_ident_byte(bytes[i - 1])) && !method_call;
        let after = i + name_bytes.len();
        let after_ok = after < bytes.len() && !is_ident_byte(bytes[after]);
        let called = after_ok
            && source[after..].starts_with('(')
            // `function test(` declares; `if (x) test(` calls.
            && !preceded_by_function_keyword(&source[..i]);
        if before_ok && after_ok && called {
            count += 1;
        }
        i = after;
    }
    count
}

fn mentions_calls(source: &str, name: &str) -> bool {
    contains_identifier(source, name)
}

fn mentions(source: &str, name: &str) -> bool {
    contains_identifier(source, name)
}

/// `obj.name` or `obj . name` - a property access, not a bare identifier.
fn mentions_property(source: &str, name: &str) -> bool {
    let pattern = format!(".{name}");
    let mut from = 0usize;
    while let Some(rel) = source[from..].find(&pattern) {
        let at = from + rel;
        let after = at + pattern.len();
        let end_ok = after >= source.len() || !is_ident_byte(source.as_bytes()[after]);
        // Skip `.parentNode`, `.topmost`: the char before `.` must not make
        // this a different property, and the char after must not extend it.
        if end_ok {
            return true;
        }
        from = after;
    }
    false
}

/// A bare identifier immediately followed by `.`, i.e. a receiver access
/// (`parent.postMessage(...)`). Distinct from [`mentions_property`], which finds
/// the *name* as a property (`window.parent`).
fn identifier_before_dot(source: &str, name: &str) -> bool {
    let pattern = format!("{name}.");
    let mut from = 0usize;
    while let Some(rel) = source[from..].find(&pattern) {
        let at = from + rel;
        let before_ok = at == 0 || !is_ident_byte(source.as_bytes()[at - 1]);
        if before_ok {
            return true;
        }
        from = at + pattern.len();
    }
    false
}

fn contains_identifier(source: &str, name: &str) -> bool {
    let bytes = source.as_bytes();
    let mut i = 0usize;
    let nb = name.as_bytes();
    while i + nb.len() <= bytes.len() {
        if &bytes[i..i + nb.len()] == nb {
            let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
            let after = i + nb.len();
            let after_ok = after >= bytes.len() || !is_ident_byte(bytes[after]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

fn preceded_by_function_keyword(before: &str) -> bool {
    let tail = before.trim_end();
    let tail = tail.strip_suffix('*').unwrap_or(tail).trim_end();
    tail.ends_with("function")
        || tail.ends_with("async function")
        || tail.ends_with("async")
}

#[cfg(test)]
mod tests {
    use super::{classify, Blocker, Capability, HarnessShape};
    use crate::source::scan;

    fn classify_text(text: &str) -> super::Classification {
        let scanned = scan(text);
        let scripts: Vec<&str> = scanned
            .scripts
            .iter()
            .filter_map(|s| s.inline.as_deref())
            .collect();
        classify(&scanned, &scripts, None, None)
    }

    #[test]
    fn counts_only_bare_calls_not_method_calls() {
        // `assert_true` is an assertion; `foo.assert_true(` is not, and
        // counting it would inflate the "has assertions" signal.
        let c = classify_text("<script>assert_true(a); obj.assert_true(b);</script>");
        assert_eq!(c.assertion_sites, 1);
    }

    #[test]
    fn a_function_declaration_is_not_a_call_site() {
        let c = classify_text("<script>function test() { return 1; }</script>");
        assert!(!c.shapes.contains(&HarnessShape::SynchronousTest));
    }

    #[test]
    fn every_testcss_entry_point_demands_an_iframe() {
        for entry in [
            "test_parsed_value",
            "test_computed_value",
            "test_invalid_value",
            "test_valid_value",
            "test_rule",
            "test_selector",
        ] {
            let c = classify_text(&format!("<script>{entry}('color', 'red', 'red');</script>"));
            assert!(
                c.capabilities.contains(&Capability::NestedBrowsingContext),
                "{entry} must be classified as needing a nested browsing context"
            );
            assert!(matches!(c.blocker, Some(Blocker::MissingEngine(_))));
        }
    }

    #[test]
    fn declaring_testcss_by_name_is_enough_to_require_an_iframe() {
        // Indirection defeats pattern matching on call sites; the script name
        // does not.
        let c = classify_text("<script src=\"/resources/testcss.js\"></script>");
        assert!(c.capabilities.contains(&Capability::NestedBrowsingContext));
    }

    #[test]
    fn an_about_blank_iframe_source_is_not_a_missing_fixture() {
        // WPT's iframe tests set `src="about:blank"`. Treating a pseudo-URL as
        // an absent file would drop real DOM tests out of the population.
        let scanned = scan("<iframe src=\"about:blank\"></iframe>");
        assert!(scanned.resources.iter().any(|r| r.url == "about:blank"));
        let dir = std::path::Path::new("/nonexistent");
        let c = classify(&scanned, &[], Some(dir), None);
        assert!(
            !matches!(c.blocker, Some(Blocker::MissingFixture(_))),
            "about:blank must not be reported as a missing fixture: {:?}",
            c.blocker
        );
    }

    #[test]
    fn a_genuinely_absent_relative_fixture_is_still_reported() {
        let scanned = scan("<link rel=\"stylesheet\" href=\"support/absent.css\">");
        let dir = std::path::Path::new("/nonexistent");
        let c = classify(&scanned, &[], Some(dir), None);
        assert!(matches!(c.blocker, Some(Blocker::MissingFixture(_))));
    }

    #[test]
    fn a_test_with_no_assertion_is_flagged_as_cannot_fail() {
        let c = classify_text("<script>var x = 1;</script>");
        assert!(c.cannot_fail());
        assert!(matches!(c.blocker, Some(Blocker::NoAssertions)));
    }

    #[test]
    fn a_reftest_with_no_script_is_not_cannot_fail() {
        let c = classify_text("<link rel=\"match\" href=\"ref.html\"><div>x</div>");
        assert!(!c.cannot_fail());
        assert!(c.shapes.contains(&HarnessShape::Reftest));
    }

    #[test]
    fn a_plain_synchronous_test_is_executable() {
        let c = classify_text("<script>test(() => { assert_equals(1, 1); }, 'trivial');</script>");
        assert!(c.is_executable(), "{:?}", c.blocker);
    }

    #[test]
    fn all_four_required_harness_shapes_are_detected() {
        for (src, shape) in [
            ("test(function(){},'n')", HarnessShape::SynchronousTest),
            ("promise_test(function(){},'n')", HarnessShape::PromiseTest),
            ("async_test(function(t){t.done()})", HarnessShape::AsyncTest),
            ("assert_throws_js(function(){})", HarnessShape::AssertThrowsJs),
        ] {
            let c = classify_text(&format!("<script>{src}</script>"));
            assert!(c.shapes.contains(&shape), "{src} -> {:?}", c.shapes);
            assert!(shape.is_supported());
        }
    }

    #[test]
    fn legacy_setup_harness_is_detected_and_supported() {
        // The last round asserted the opposite. `testharness.js` at the pinned
        // revision implements `setup` (`expose(setup, 'setup')`, line 1294) and
        // this runner loads that harness rather than reimplementing it, so the
        // shape works. Asserting it is unsupported blocked 772 tests and
        // mislabelled a working feature as a gap in this runner.
        let c = classify_text("<script>setup(function() { assert_true(1); });</script>");
        assert!(c.shapes.contains(&HarnessShape::LegacySetup));
        assert!(
            HarnessShape::LegacySetup.is_supported(),
            "the pinned testharness.js implements setup(); blocking it is a false gap"
        );
        assert!(
            !matches!(c.blocker, Some(Blocker::UnsupportedShape(_))),
            "setup() must not block a test: {:?}",
            c.blocker
        );
    }

    #[test]
    fn no_harness_shape_is_blocked_for_being_old() {
        // The general rule behind the `setup` fix: support is a property of this
        // runner, not of the shape's age. Every shape this runner drives is
        // driven by loading WPT's own harness, so all of them are supported and
        // the only way a shape blocks is by being one this runner cannot load.
        for shape in [
            HarnessShape::SynchronousTest,
            HarnessShape::PromiseTest,
            HarnessShape::AsyncTest,
            HarnessShape::AssertThrowsJs,
            HarnessShape::LegacySetup,
        ] {
            assert!(shape.is_supported(), "{shape:?} must be supported");
        }
    }

    #[test]
    fn an_unscannable_file_is_never_reported_executable() {
        let c = classify_text("<script>var a = 1;");
        assert!(!c.is_executable());
        assert!(matches!(c.blocker, Some(Blocker::Unscannable(_))));
    }

    #[test]
    fn window_parent_is_a_proxy_not_an_identifier() {
        let c = classify_text("<script>test(function(){ assert_true(parent.ok); },'n');</script>");
        assert!(c.capabilities.contains(&Capability::WindowProxy));
    }

    #[test]
    fn timers_are_available_and_do_not_block() {
        let c = classify_text("<script>async_test(function(t){ setTimeout(t.done,0); });</script>");
        assert!(c.capabilities.contains(&Capability::Timers));
        assert!(c.is_executable(), "{:?}", c.blocker);
    }
}
