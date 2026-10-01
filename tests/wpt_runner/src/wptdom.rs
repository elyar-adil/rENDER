//! Execute WPT `dom/` and `html/` tests against the engine, for real.
//!
//! # The design constraint that shapes everything here
//!
//! **The harness is WPT's, not ours.** `/resources/testharness.js` is loaded
//! from the pinned checkout and executed unmodified by the engine's own
//! JavaScript runtime. The runner does not reimplement `test()`,
//! `assert_equals()`, `promise_test()` or `async_test()`, and it does not
//! interpret a test's source to decide whether it passed.
//!
//! That is not a stylistic preference. The alternative - statically scanning a
//! test for assertion calls and calling it a pass - produces a number that
//! measures the scanner, and it produces a *high* one, because a scanner
//! recognises assertions it has seen and ignores the ones that would fail. A
//! conformance figure produced that way is worse than no figure, because it is
//! quotable.
//!
//! So the only thing standing between this runner and a number is whether the
//! engine can run the harness, and [`crate::probe`] measures exactly that.
//!
//! # How a verdict is read back
//!
//! Through `add_result_callback`, which `testharness.js` exposes as a global
//! specifically so that an out-of-process driver can receive results. The
//! callback receives the harness's own `Test` object with its `status` and its
//! `message`, so what this runner records is the harness's verdict on the
//! test, not this runner's opinion of it.
//!
//! `tests` itself is *not* a global - it is a `var` inside the harness's IIFE -
//! and an earlier draft of this module looked for it there and found nothing.
//! That is worth recording: reading the wrong channel produces a confident
//! "no results" that looks like an engine failure and is not.
//!
//! # What is measured and what is refused
//!
//! Three limits, each a bound rather than a preference, because a WPT document
//! is hostile input and a hang is a truncated result file that looks like a
//! smaller suite:
//!
//! * microtask and timer turns are drained to a fixed bound;
//! * a per-test step budget comes from the engine's own `RuntimeLimits`;
//! * an empty event loop is the signal that the test finished, and a test that
//!   never finishes is a `harness-limitation`, never a fail.

use std::collections::BTreeMap;
use std::path::Path;

use render_core::html::parse_document;
use render_core::js::{CompiledScript, JsRuntime, RuntimeLimits, TimerRequest};

use crate::classify::Capability;
use crate::engine::{Engine, HarnessError, TestContext, Verdict};
use crate::mechanism::{self, Category};
use crate::results::FileResult;

/// What this adapter is, stated in the results file.
pub const ENGINE_DESCRIPTION: &str = concat!(
    "render-core + render-js: WPT testharness.js executed unmodified in the engine's own ",
    "realm, results read through add_result_callback"
);

/// Maximum microtask and timer turns drained per test.
///
/// Not a preference. `promise_test` and `async_test` need their queues drained,
/// and a test that never settles would otherwise spin forever. The bound turns
/// a hang into an explained skip, which is the only honest way to end one.
const MAX_DRAIN_TURNS: usize = 512;

/// A test outcome as `testharness.js` itself reports it.
///
/// The five statuses are the harness's, from `Test.statuses` in
/// `testharness.js`:
///
/// ```text
/// PASS: 0, FAIL: 1, TIMEOUT: 2, NOTRUN: 3, PRECONDITION_FAILED: 4
/// ```
///
/// `PRECONDITION_FAILED` deserves its own arm because it is *not* a failure.
/// It means the test declared a precondition the engine did not meet - usually
/// `promise_test` with `optional: true` on a browser that lacks the feature -
/// and WPT counts it as neither pass nor fail. Folding it into either is how a
/// skip rate turns into a false conformance claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarnessStatus {
    Pass,
    Fail,
    Timeout,
    NotRun,
    /// A declared precondition was not met. Not a failure.
    PreconditionFailed,
    /// The harness reported a status this runner does not know. Refused rather
    /// than guessed, because guessing here would put an unknown into the
    /// numerator or the denominator.
    Unknown,
}

impl HarnessStatus {
    fn from_number(value: f64) -> Self {
        if value == 0.0 {
            Self::Pass
        } else if value == 1.0 {
            Self::Fail
        } else if value == 2.0 {
            Self::Timeout
        } else if value == 3.0 {
            Self::NotRun
        } else if value == 4.0 {
            Self::PreconditionFailed
        } else {
            Self::Unknown
        }
    }
}

/// The result of one executed test.
#[derive(Clone, Debug)]
pub struct Execution {
    pub status: HarnessStatus,
    /// The test's name, as the harness recorded it.
    pub name: String,
    /// Assertions the harness recorded evaluating, from `Tests.asserts_run`.
    ///
    /// Read from the harness's own record rather than counted here. It is the
    /// only field that says whether a pass is real, and getting the *name* wrong
    /// made every pass in one run vacuous while looking completely healthy -
    /// see [`RESULT_SINK`].
    pub harness_asserts: u32,
    /// The harness's failure message, when it gave one.
    pub message: Option<String>,
    /// How many assertions the harness reported evaluating.
    pub assertions: u32,
    /// The mechanism key, for the ranked table. See [`classify_error`].
    pub mechanism: Option<String>,
    /// The category this result falls into.
    pub category: Category,
}

/// The `render-core` + `render-js` adapter.
pub struct WptHarnessEngine {
    /// Compiled once per process: 198 KB of harness parsed per test would make a
    /// 30,000-test run take hours for no reason.
    harness: CompiledScript,
    limits: RuntimeLimits,
    /// Number of `PASS` results seen, so a run that suddenly passes everything
    /// is visible in the output rather than only in the numerator.
    passed: u32,
}

impl WptHarnessEngine {
    /// Compile the harness from the pinned checkout.
    ///
    /// # Errors
    ///
    /// When the harness is absent or does not compile. Both are reported as
    /// errors rather than as a capability gap, because a harness that will not
    /// load is a statement about this host, not about the engine's DOM.
    pub fn new(suite_root: &Path) -> Result<Self, HarnessError> {
        let path = suite_root.join("resources/testharness.js");
        let source = std::fs::read_to_string(&path).map_err(|error| HarnessError {
            message: format!("could not read {}: {error}", path.display()),
        })?;
        let limits = RuntimeLimits::default();
        let harness = CompiledScript::compile(&source, &limits).map_err(|error| HarnessError {
            message: format!("testharness.js did not compile: {error}"),
        })?;
        Ok(Self {
            harness,
            limits,
            passed: 0,
        })
    }

    #[must_use]
    pub const fn passed(&self) -> u32 {
        self.passed
    }
}

impl Engine for WptHarnessEngine {
    fn describe(&self) -> String {
        format!("{ENGINE_DESCRIPTION} (drain up to {MAX_DRAIN_TURNS} turns)")
    }

    /// # Errors
    ///
    /// Only for harness-level failure: the test file could not be read, or the
    /// runtime could not be constructed. An engine limitation is
    /// [`Verdict::Unsupported`], a skip.
    fn run(&mut self, ctx: &TestContext<'_>) -> Result<Verdict, HarnessError> {
        // Refuse before doing any work. A test that needs a nested browsing
        // context cannot be driven by anything this adapter does, and a test
        // that drives an iframe would otherwise half-run and report a verdict
        // about the wrong document.
        if let Some(capability) = first_unsupported_capability(ctx) {
            return Ok(Verdict::Unsupported {
                capability: Some(capability),
                reason: format!(
                    "render-core implements no nested browsing context, so {} cannot be driven",
                    capability.label()
                ),
            });
        }

        let execution = self.execute_test(ctx)?;
        Ok(execution.into_verdict(ctx))
    }
}

impl WptHarnessEngine {
    /// Run one test and report what the harness said.
    fn execute_test(&mut self, ctx: &TestContext<'_>) -> Result<Execution, HarnessError> {
        let scan = crate::source::scan(ctx.source);
        let inline: Vec<String> = scan
            .scripts
            .iter()
            .filter_map(|s| s.inline.clone())
            .collect();

        // A fresh document per test, from the test's own HTML. Sharing one
        // document between tests would let state leak and turn an engine
        // ordering bug into a hundred apparent failures.
        let mut parsed = parse_document(ctx.source);
        let mut runtime = JsRuntime::with_limits(&parsed.dom, self.limits);

        // 1. Load WPT's harness, unmodified, from the pinned checkout.
        runtime
            .execute_compiled(&mut parsed.dom, &self.harness)
            .map_err(|error| HarnessError {
                message: format!("testharness.js did not execute: {error}"),
            })?;

        // 2. Register the result channel *before* the test runs. The harness
        //    exposes `add_result_callback` precisely so an out-of-process driver
        //    can do this, and registering afterwards would miss every result
        //    from a synchronously-run test.
        runtime
            .execute(
                &mut parsed.dom,
                RESULT_SINK,
            )
            .map_err(|error| HarnessError {
                message: format!("could not register the result callback: {error}"),
            })?;

        // 3. Run the test's own scripts, in document order.
        for script in &inline {
            if let Err(error) = runtime.execute(&mut parsed.dom, script) {
                // A script that throws is what the harness's window `error`
                // handler is for, and the harness will have recorded it. Only a
                // *resource limit* aborts the run, because a step budget that
                // runs out means the rest of the test would be measuring a
                // truncated engine rather than a wrong one.
                if error.kind() == render_core::js::JsErrorKind::ResourceLimit {
                    return Ok(Execution {
                        status: HarnessStatus::NotRun,
                        name: ctx.path.to_owned(),
                        message: Some(format!("engine step budget exhausted: {error}")),
                        assertions: 0,
                        harness_asserts: 0,
                        mechanism: Some("engine-step-budget".to_owned()),
                        category: Category::HarnessLimitation,
                    });
                }
            }
        }

        // 4. Drain the queues. `promise_test` and `async_test` finish in
        //    microtasks and timers, and stopping before they run would report
        //    NOTRUN for every async test - a systematic under-count that looks
        //    like an engine that cannot do async at all.
        let drained = self.drain(&mut runtime, &mut parsed.dom);
        if !drained.settled {
            return Ok(Execution {
                status: HarnessStatus::NotRun,
                name: ctx.path.to_owned(),
                message: Some(format!(
                    "the event loop did not settle within {MAX_DRAIN_TURNS} turns"
                )),
                assertions: 0,
                harness_asserts: 0,
                mechanism: Some("event-loop-never-settles".to_owned()),
                category: Category::HarnessLimitation,
            });
        }

        Ok(read_results(&mut runtime, &mut parsed.dom, ctx.path))
    }

    /// Drain microtasks and timers to a fixed bound.
    ///
    /// Returns whether the loop settled. A loop that never settles is a harness
    /// limitation, never a fail: the test did not assert anything wrong, it
    /// simply never finished, and those are different claims.
    fn drain(
        &mut self,
        runtime: &mut JsRuntime,
        dom: &mut render_core::dom::Dom,
    ) -> DrainOutcome {
        for _ in 0..MAX_DRAIN_TURNS {
            let mut did_work = false;

            for microtask in runtime.take_pending_microtasks() {
                did_work = true;
                // A rejection inside a microtask is the test's business; the
                // harness's `unhandledrejection` handler records it. Swallowing
                // it here is correct precisely because something downstream
                // does account for it.
                let _ = runtime.invoke_microtask(dom, microtask);
            }

            let requests = runtime.take_pending_timer_requests();
            let mut fireable: Vec<u64> = Vec::new();
            for request in requests {
                match request {
                    TimerRequest::Schedule { id, .. } => fireable.push(id),
                    TimerRequest::Cancel { .. } => {}
                }
            }
            for id in fireable {
                did_work = true;
                let _ = runtime.fire_timer(dom, id);
            }

            if !did_work {
                return DrainOutcome { settled: true };
            }
        }
        DrainOutcome { settled: false }
    }
}

struct DrainOutcome {
    settled: bool,
}

/// The script that installs the result channel.
///
/// Reads the harness's own `Test` object fields rather than re-deriving status
/// from a message string, because the numeric status is the harness's contract
/// and a message format is not.
///
/// The collected results are held as a **JSON string**, not as a JS array, and
/// that is forced by the public surface rather than chosen: `JsValue` has no
/// `Array` variant and the object's array-ness lives in a `pub(crate)` field, so
/// a Rust-side array walk is not available from outside the engine. Serialising
/// in the realm and parsing the string in Rust uses nothing private, and it has
/// the side benefit that a harness error inside the sink cannot silently produce
/// a half-read result.
const RESULT_SINK: &str = r#"
(function () {
    // Disable the harness's on-page report BEFORE any test runs.
    //
    // `setup({output: false})` is a documented `testharness.js` property
    // (documented at the `setup` JSDoc, line ~1096) that exists for exactly
    // this: a runner collecting results out-of-band. It suppresses the
    // `Output` object, which otherwise calls `document.createElementNS` to
    // build a results table. The engine does not implement `createElementNS`,
    // and an earlier draft of this runner therefore saw every test throw
    // "Cannot read properties of undefined (reading 'id')" and record nothing.
    //
    // This is NOT the same as reimplementing the harness, and the difference
    // matters. `testcss.js`'s iframe is the *subject* of the test: the test is
    // that a fresh document with a fresh stylesheet computes a value a certain
    // way, and removing the iframe removes the test. The harness's output
    // object is a *display* concern: it renders verdicts that have already been
    // decided. Turning it off cannot change whether `assert_equals(1, 2)` holds.
    //
    // And it is a one-way door, so it cannot be re-enabled by a test: the
    // harness computes `this.enabled = this.enabled && (...)`, so once false it
    // stays false for the life of the realm.
    setup({ output: false });

    var collected = [];
    // The assertion count, obtained by wrapping every assertion entry point.
    //
    // Two drafts failed here, and both are worth recording because the second
    // one looked correct:
    //
    //   1. Reading `test.asserts`. That field does not exist in
    //      `testharness.js`. Every value came back `undefined` and became 0, so
    //      the run produced **210 passes and all 210 were vacuous** - a clean
    //      looking 7.4% conformance rate made entirely of tests that asserted
    //      nothing. Nothing errored. That is the single most dangerous result
    //      this crate exists to prevent.
    //   2. Reading `tests.asserts_run`, which *does* exist and is the harness's
    //      own record of every assertion it ran. But `tests` is a `var` **inside
    //      testharness.js's IIFE**, so it is not reachable from here, and every
    //      test then failed with "tests is not defined". The negative control
    //      caught that in one run, which is the control doing its job.
    //
    // Wrapping is the version that works, and it is honest in a way reading a
    // field would not be: it counts the assertions the engine actually
    // *evaluated*, which is the quantity a pass rate depends on. Wrapping rather
    // than replacing leaves WPT's own logic underneath, so a wrapped assertion
    // still throws and the harness still catches it and sets the status.
    var counted = 0;
    ["assert_equals", "assert_not_equals", "assert_true", "assert_false",
     "assert_array_equals", "assert_object_equals", "assert_approx_equals",
     "assert_unreached", "assert_regexp_match", "assert_in_array",
     "assert_class_string", "assert_own_property", "assert_less_than",
     "assert_greater_than", "assert_throws_js", "assert_throws_dom",
     "assert_raises_js", "promise_rejects_js"]
        .forEach(function (name) {
            var original = globalThis[name];
            if (typeof original !== "function") { return; }
            var wrapper = function () {
                counted += 1;
                return original.apply(this, arguments);
            };
            wrapper.name = name;
            globalThis[name] = wrapper;
        });

    function harvest(entry) {
        var test = entry.test || entry;
        collected.push({
            name: String(test.name),
            status: Number(test.status),
            message: test.message === undefined || test.message === null
                ? null : String(test.message),
            asserts: counted
        });
    }
    add_result_callback(harvest);
    // `add_completion_callback` receives the harness-wide status, which is what
    // catches a failure that happened *outside* any test - an exception during
    // script evaluation, for instance. Without it a test that dies before
    // registering produces no results at all and reads as "no evidence" rather
    // than as a harness-level error.
    var completion = null;
    add_completion_callback(function (tests, status) {
        completion = {
            status: status && status.status !== undefined ? Number(status.status) : -1,
            message: status && status.message ? String(status.message) : null,
            num_tests: tests && tests.length ? tests.length : 0
        };
    });
    // One function the Rust side calls, so the read is a single script
    // execution rather than a walk through crate-private object internals.
    globalThis.__wpt_read = function () {
        return JSON.stringify({ results: collected, completion: completion });
    };
})();
"#;

/// The shape of what [`RESULT_SINK`] serialises.
///
/// Hand-parsed rather than deserialised, because this crate carries no
/// dependencies and the format is four fields wide. A tolerant parser that
/// returns `None` for anything it does not recognise is the right failure mode:
/// an unparseable result must become a `HarnessLimitation`, never a guess.
#[derive(Debug, Default)]
struct Harvest {
    results: Vec<HarvestedTest>,
    completion: Option<HarvestedCompletion>,
}

#[derive(Debug)]
struct HarvestedTest {
    name: String,
    status: f64,
    message: Option<String>,
    asserts: u32,
}

#[derive(Debug)]
struct HarvestedCompletion {
    status: f64,
    message: Option<String>,
    tests: u32,
}

impl Harvest {
    /// Parse the JSON the sink produced.
    #[must_use]
    fn parse(json: &str) -> Option<Self> {
        let mut out = Self::default();
        let bytes = json.as_bytes();

        // `results` array.
        let mut i = after_key(json, "results")?;
        if bytes.get(i) != Some(&b'[') {
            return None;
        }
        i += 1;
        while let Some(object) = take_object(bytes, &mut i) {
            out.results.push(parse_test(&object));
        }

        // `completion` object, which may be `null`.
        let mut i = after_key(json, "completion")?;
        if bytes.get(i) == Some(&b'{') {
            let object = take_object(bytes, &mut i)?;
            out.completion = Some(HarvestedCompletion {
                status: number_field(&object, "status").unwrap_or(-1.0),
                message: string_field(&object, "message"),
                tests: number_field(&object, "num_tests").unwrap_or(0.0).max(0.0) as u32,
            });
        }
        Some(out)
    }
}

/// Skip past `"key":` and any whitespace, returning the index of the value.
///
/// The `:` has to be stepped over explicitly. An earlier draft returned the
/// index of the `:` itself and every parse returned `None` - a reader that
/// refuses *everything* is indistinguishable from a suite that produced
/// nothing, which is the exact ambiguity this crate refuses to leave open.
/// The test `a_harvest_round_trips_the_harness_shape` is what caught it.
fn after_key(json: &str, key: &str) -> Option<usize> {
    let needle = format!("\"{key}\":");
    let at = json.find(&needle)? + needle.len();
    let bytes = json.as_bytes();
    let mut i = at;
    skip_ws(bytes, &mut i);
    Some(i)
}

fn skip_ws(bytes: &[u8], i: &mut usize) {
    while matches!(bytes.get(*i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        *i += 1;
    }
}

/// Take the next `{...}` from the stream, honouring string literals so a brace
/// inside a test's failure message does not end the object early.
fn take_object(bytes: &[u8], i: &mut usize) -> Option<String> {
    skip_ws(bytes, i);
    if bytes.get(*i) != Some(&b'{') {
        return None;
    }
    let start = *i;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    while *i < bytes.len() {
        let byte = bytes[*i];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        *i += 1;
                        return Some(String::from_utf8_lossy(&bytes[start..*i]).into_owned());
                    }
                }
                b']' if depth == 0 => return None,
                _ => {}
            }
        }
        *i += 1;
    }
    None
}

/// Extract one field from a serialised result object.
///
/// Returns an owned `String` for a string field (because unescaping allocates)
/// and a borrowed slice for a numeric one. The `None` cases - absent, `null`,
/// and empty string - are collapsed deliberately: for every field this reads, a
/// null and an empty string mean the same thing, and treating them differently
/// would need a third case in every caller for no gain.
fn field<'a>(object: &'a str, key: &str) -> Option<std::borrow::Cow<'a, str>> {
    let needle = format!("\"{key}\":");
    let at = object.find(&needle)? + needle.len();
    let rest = &object[at..];
    let bytes = rest.as_bytes();
    if bytes.first() == Some(&b'"') {
        let mut out = String::new();
        let mut chars = rest[1..].chars();
        let mut escaped = false;
        for ch in chars.by_ref() {
            if escaped {
                // Only the escapes `JSON.stringify` actually emits for the
                // characters a WPT failure message contains.
                out.push(match ch {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    other => other,
                });
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                break;
            } else {
                out.push(ch);
            }
        }
        return if out.is_empty() { None } else { Some(std::borrow::Cow::Owned(out)) };
    }
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    let value = rest[..end].trim();
    if value.is_empty() || value == "null" {
        None
    } else {
        Some(std::borrow::Cow::Borrowed(value))
    }
}

fn number_field(object: &str, key: &str) -> Option<f64> {
    field(object, key).and_then(|v| v.parse().ok())
}

fn string_field(object: &str, key: &str) -> Option<String> {
    field(object, key).map(std::borrow::Cow::into_owned)
}
fn parse_test(object: &str) -> HarvestedTest {
    HarvestedTest {
        name: string_field(object, "name").unwrap_or_else(|| "<unnamed>".to_owned()),
        status: number_field(object, "status").unwrap_or(-1.0),
        message: string_field(object, "message"),
        asserts: number_field(object, "asserts").unwrap_or(0.0).max(0.0) as u32,
    }
}

/// Read the collected results out of the realm.
///
/// Runs the sink's reader, which returns a JSON string. A reader that throws, or
/// returns something that will not parse, becomes a [`HarnessStatus::Unknown`]
/// and therefore an explained skip - never a pass and never a fail.
fn read_results(runtime: &mut JsRuntime, dom: &mut render_core::dom::Dom, path: &str) -> Execution {
    let json = runtime
        .execute(dom, "String(globalThis.__wpt_read())")
        .ok()
        .map(|outcome| outcome.value.to_js_string());

    let harvest = json.as_deref().and_then(Harvest::parse);

    let Some(harvest) = harvest else {
        return Execution {
            status: HarnessStatus::Unknown,
            name: path.to_owned(),
            message: Some(format!(
                "the result channel did not produce a readable harvest: {}",
                json.as_deref().unwrap_or("<the reader threw>")
            )),
            assertions: 0,
            harness_asserts: 0,
            mechanism: Some("result-channel-unreadable".to_owned()),
            category: Category::HarnessLimitation,
        };
    };

    if harvest.results.is_empty() {
        // No result at all. That is a distinct fact from "failed": either the
        // test registered nothing, or the harness-level status recorded why.
        let (status, message) = match &harvest.completion {
            Some(completion) => (Some(completion.status), completion.message.clone()),
            None => (None, None),
        };
        // The harness's own count of tests it ran, kept because a *disagreement*
        // between this and the number of results is the cheapest available
        // signal that a result went missing. It is reported rather than
        // consumed, so the disagreement stays visible instead of being resolved
        // by whichever number happened to be read first.
        let registered = harvest
            .completion
            .as_ref()
            .map_or(0, |c| c.tests)
            .saturating_sub(harvest.results.len() as u32);
        return Execution {
            status: match status {
                Some(value) => HarnessStatus::from_number(value),
                // A test that produced no result and no harness status is a
                // harness limitation: this runner could not obtain a verdict,
                // and claiming a fail would manufacture one.
                None => HarnessStatus::Unknown,
            },
            name: path.to_owned(),
            message: Some(match message {
                Some(given) => format!(
                    "{given} (the harness reported {registered} test(s) and {} result(s) \
                     reached this runner)",
                    harvest.results.len()
                ),
                None => format!(
                    "the harness reported no result and no status for this test ({registered} \
                     registered, 0 collected)"
                ),
            }),
            assertions: 0,
            harness_asserts: 0,
            mechanism: Some("no-verdict-from-harness".to_owned()),
            category: Category::HarnessLimitation,
        };
    }
    // Several results means subtests. The file's verdict is the worst of them:
    // a file where one subtest passed and another failed did not pass, and
    // averaging them would be a number nobody could act on.
    let mut worst = Execution {
        status: HarnessStatus::Pass,
        name: path.to_owned(),
        message: None,
        assertions: 0,
        harness_asserts: 0,
        mechanism: None,
        category: Category::HarnessLimitation,
    };
    for test in harvest.results {
        let status = HarnessStatus::from_number(test.status);
        // `asserts_run` is cumulative across the whole file, so it is taken from
        // the last result rather than summed - summing would multiply it by the
        // number of subtests.
        worst.harness_asserts = test.asserts;
        worst.assertions = worst.assertions.saturating_add(test.asserts);
        if severity(status) > severity(worst.status) {
            worst.status = status;
            worst.name = test.name;
            worst.message = test.message;
        }
    }
    worst.category = match worst.status {
        HarnessStatus::Pass => Category::AcceptableDifference,
        HarnessStatus::PreconditionFailed | HarnessStatus::NotRun | HarnessStatus::Timeout => {
            Category::UnimplementedFeature
        }
        HarnessStatus::Fail | HarnessStatus::Unknown => Category::EngineDefect,
    };
    worst
}

/// How bad a status is, for the "worst of the subtests" rule.
///
/// `Fail` outranks every non-failure, so a file containing one real failure
/// alongside several skips is still recorded as a failure. `Unknown` outranks
/// `Fail` because the only safe reading of a verdict this runner cannot
/// interpret is "make no claim", and a claim would be worse than a defect.
///
/// `NotRun` outranks `PreconditionFailed`: the first means the harness lost the
/// thread, the second means the test ran and declined deliberately. Only the
/// second is a signal, and treating a lost thread as a deliberate decline would
/// be a false claim about the test.
const fn severity(status: HarnessStatus) -> u8 {
    match status {
        HarnessStatus::Pass => 0,
        HarnessStatus::PreconditionFailed => 1,
        HarnessStatus::NotRun => 2,
        HarnessStatus::Timeout => 3,
        HarnessStatus::Fail => 4,
        HarnessStatus::Unknown => 5,
    }
}

impl Execution {
    /// Map onto the four-state model.
    ///
    /// This mapping is where the four categories are separated, and the one that
    /// matters most is `Fail` -> [`Verdict::Failed`]. A `fail` produced here is
    /// a real assertion that the engine's own DOM disagreed with WPT's
    /// expectation, which is exactly what an engine defect is. Nothing else in
    /// this file is allowed to produce a `Fail`.
    fn into_verdict(self, ctx: &TestContext<'_>) -> Verdict {
        match self.status {
            // A pass is a pass only if the harness actually evaluated something.
            //
            // The measured run this replaces reported 210 passes, and **all 210
            // evaluated zero assertions**: the sink was reading `test.asserts`,
            // a field `testharness.js` does not define, so every count came back
            // `undefined` and became 0. The number looked entirely healthy - a
            // 7.4% conformance rate - and every single one of those passes was
            // vacuous. `Report::vacuous_passes` flagged them in a footnote, which
            // is nowhere near loud enough for a 100% vacuous pass rate.
            //
            // So a pass with zero evaluated assertions is **not** a pass here. It
            // becomes an explained skip. That is the conservative direction: it
            // can only lower the reported rate, never raise it, and a lower rate
            // that is real is worth more than a higher one that is not.
            HarnessStatus::Pass if self.assertions == 0 => Verdict::Unsupported {
                capability: None,
                reason: format!(
                    "the harness reported PASS but evaluated 0 assertions, while this file \
                     declares {} assertion site(s) statically. A pass that checked nothing is \
                     not evidence about the engine, so it is not counted as one.",
                    ctx.assertion_sites
                ),
            },
            HarnessStatus::Pass => Verdict::Passed {
                assertions_evaluated: self.assertions,
                notes: self.notes(),
            },
            HarnessStatus::Fail => Verdict::Failed {
                assertion: format!(
                    "{}: {}",
                    self.name,
                    self.message.as_deref().unwrap_or("<the harness gave no message>")
                ),
                // Not fabricated. The engine does not surface the source
                // location of a DOM method, so a guessed `file:line` would be a
                // confident pointer at the wrong code. See FINDINGS.md.
                engine_location: None,
                notes: self.notes(),
            },
            HarnessStatus::PreconditionFailed => Verdict::Unsupported {
                capability: None,
                reason: format!(
                    "the test declared a precondition this engine does not meet: {}",
                    self.message.as_deref().unwrap_or("<no message>")
                ),
            },
            HarnessStatus::NotRun => Verdict::Unsupported {
                capability: None,
                reason: format!(
                    "the test did not run to a verdict: {}",
                    self.message.as_deref().unwrap_or("<no message>")
                ),
            },
            HarnessStatus::Timeout => Verdict::Unsupported {
                capability: None,
                reason: "the test did not complete within the drain bound".to_owned(),
            },
            // An unknown status is the case where guessing would be most
            // tempting and most dangerous. It becomes an explained skip, which
            // is the one bucket that cannot inflate either the numerator or
            // the denominator.
            HarnessStatus::Unknown => Verdict::Unsupported {
                capability: None,
                reason: format!(
                    "the harness reported no verdict this runner can interpret: {}",
                    self.message.as_deref().unwrap_or("<no message>")
                ),
            },
        }
        .with_ctx(ctx)
    }

    fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.status == HarnessStatus::Pass && self.assertions == 0 {
            notes.push(
                "the harness reported PASS having evaluated zero assertions; counted as a vacuous \
                 pass"
                    .to_owned(),
            );
        }
        notes
    }
}

/// A small extension so `into_verdict` reads as one expression.
trait WithCtx {
    fn with_ctx(self, ctx: &TestContext<'_>) -> Verdict;
}

impl WithCtx for Verdict {
    fn with_ctx(self, _ctx: &TestContext<'_>) -> Verdict {
        self
    }
}

/// Turn a JavaScript error into a mechanism key.
///
/// The point is that each distinct cause gets a *name*, because a mechanism
/// table of unnamed buckets is a mechanism table nobody can act on. Grouping by
/// the error's `kind` and its leading token is crude but stable, and stability
/// is what a ranked table needs: a table whose keys change every run cannot be
/// diffed, and a table that cannot be diffed cannot show progress.
#[must_use]
pub fn classify_error(error: &render_core::js::JsError) -> (String, String) {
    let kind = format!("{:?}", error.kind()).to_ascii_lowercase();
    let message = error.message();
    // The first sentence of the message is the most stable part; WPT's own
    // messages are template literals with the varying detail at the end.
    let head = message.split(['.', ':']).next().unwrap_or("").trim();
    let key = if head.is_empty() {
        format!("js-{kind}")
    } else {
        format!("js-{kind}-{}", slug(head))
    };
    (key, format!("the JavaScript runtime raised {kind}: {head}"))
}

/// Lowercase, hyphenated, length-capped form of a message head, for a stable key.
fn slug(text: &str) -> String {
    mechanism::slug(text)
}

/// Which required capability, if any, makes this test undrivable.
fn first_unsupported_capability(ctx: &TestContext<'_>) -> Option<Capability> {
    let scan = crate::source::scan(ctx.source);
    let inline: Vec<&str> = scan
        .scripts
        .iter()
        .filter_map(|s| s.inline.as_deref())
        .collect();
    let classification = crate::classify::classify(&scan, &inline, None, None);
    classification
        .capabilities
        .iter()
        .copied()
        .find(|c| !crate::classify::capability_is_available(*c))
}

/// Per-area tallies, for the results file.
pub type AreaTally = BTreeMap<String, u32>;

/// Execute every test in the named areas, recording a row for every one.
///
/// Generic over the adapter so that the negative control can drive the *same*
/// pipeline with a deliberately wrong engine. That is the whole point: a
/// control that exercises a different code path from the real run proves
/// nothing about the real run.
///
/// # Every file gets a row
///
/// Including the ones declined with a reason. A file silently absent from the
/// output is indistinguishable from a file that was never attempted, and that
/// ambiguity is how a run under-reports. The census is the other half of this:
/// it says how many files exist, and this says how many were accounted for, and
/// the two are compared.
/// Which adapter drives the pipeline.
///
/// Two values, not a trait object, because the negative control has to traverse
/// the *same* code as a real run - including the census veto that demotes an
/// adapter's pass to a skip - and a trait object would let a future caller swap
/// the pipeline itself. One enum with two arms is a smaller surface than a
/// generic parameter and cannot be extended by accident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapter {
    /// The real one: WPT's own harness, executed by the engine.
    Real,
    /// A negative control that reports pass for every test and asserts nothing.
    ///
    /// Present in shipped code on purpose. A control that only exists as a test
    /// is a control nobody runs, and the failure it guards against - a 100%
    /// rate that looks healthy - is exactly the kind that survives review.
    Control,
}

impl Adapter {
    const fn label(self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::Control => "negative-control",
        }
    }
}

/// An adapter that cannot fail. Every test passes, and every pass evaluated zero
/// assertions - which is the shape of the bug this crate exists to catch,
/// injected deliberately.
struct AlwaysPasses;

impl Engine for AlwaysPasses {
    fn describe(&self) -> String {
        "NEGATIVE CONTROL: an adapter that reports pass for every test".to_owned()
    }
    fn run(&mut self, _ctx: &TestContext<'_>) -> Result<Verdict, HarnessError> {
        Ok(Verdict::Passed {
            assertions_evaluated: 0,
            notes: vec!["negative control: this pass asserts nothing".to_owned()],
        })
    }
}

/// Execute every test in the named areas, recording a row for every one.
pub fn execute_areas(
    areas: &[&str],
    suite_root: &Path,
    adapter: Adapter,
    limit: Option<usize>,
) -> Vec<FileResult> {
    let mut out = Vec::new();
    for area in areas {
        // Counted first so progress can be a fraction of a known total. A
        // "still running" message with no denominator is the same sin as a
        // percentage without one: a reader cannot tell a slow run from a stuck
        // one, and a run that looks stuck gets killed, losing the whole result.
        let paths = walk_area(suite_root, area);
        let total = paths.len();
        let mut done = 0usize;
        for path in paths {
            if let Some(limit) = limit {
                if out.len() >= limit {
                    return out;
                }
            }
            out.push(run_isolated(&path, area, suite_root, adapter));
            done += 1;
            // Every 50, and on the first and last, so a long run is visibly
            // alive without producing thousands of lines of noise.
            if done == 1 || done == total || done % 50 == 0 {
                eprintln!("  {area}: {done}/{total}");
            }
        }
    }
    out
}

/// Native stack given to each test's interpreter thread.
///
/// # Why this exists, and why it is not papering over a defect
///
/// A stack overflow **aborts the process**. It is not a panic, so
/// `catch_unwind` cannot contain it, and the defence in [`crate::engine`] against
/// a truncated result file simply does not apply. The first full run of
/// `dom/`+`html/` died exactly that way — `thread 'main' has overflowed its
/// stack`, no results file, and nothing to say which test did it.
///
/// The cause is a disagreement between two limits that do not know about each
/// other. `RuntimeLimits::max_call_depth` is 4,096 interpreter frames, and
/// `render-js` budgets roughly 2 KiB of native stack per frame, so the engine
/// assumes a stack of several hundred megabytes. The main thread on this
/// platform has 1 MiB. The engine's own recursion guard is therefore
/// **unreachable**: the process dies before the guard can fire, so the limit
/// that exists to prevent this cannot prevent it.
///
/// Handing the interpreter a stack large enough to *reach* the engine's own
/// guard is supplying the precondition that guard depends on, not hiding a bug.
/// The engine still enforces 4,096 frames; this just makes the enforcement
/// reachable. And it does not conceal a genuine runaway: such a test still
/// overflows and still produces no result, which remains an engine finding and is
/// reported as one.
pub const TEST_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Run one test on a thread with a stack the engine's call-depth limit can be
/// reached on.
///
/// The harness is recompiled per test rather than shared, which costs 198 KB of
/// parsing per test and is paid deliberately: a realm that outlives a test is a
/// realm carrying that test's mutated DOM into the next one, and a run where
/// test N's state reaches test N+1 measures the harness.
fn run_isolated(path: &str, area: &str, suite_root: &Path, adapter: Adapter) -> FileResult {
    let full = suite_root.join(path);
    let source = match std::fs::read_to_string(&full) {
        Ok(source) => source,
        Err(err) => {
            return harness_error(
                path,
                area,
                format!("could not read {path}: {err}"),
                "test-file-unreadable",
            );
        }
    };

    // Kept as clones for the fallible paths below, which run *after* the closure
    // has taken the originals. A second `to_owned` here would be a fix for a
    // borrow error and not for anything about the run.
    let reported_path = path.to_owned();
    let reported_area = area.to_owned();
    let path = path.to_owned();
    let area = area.to_owned();
    let suite_root = suite_root.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name(format!("wpt-{}-{}", adapter.label(), mechanism::slug(&path)))
        .stack_size(TEST_STACK_BYTES)
        .spawn(move || {
            // The control still has to load the real harness. It is proving that
            // the *pipeline* mishandles a wrong adapter, not that a run without
            // a harness is nonsense, and skipping the load would quietly test a
            // different thing.
            if WptHarnessEngine::new(&suite_root).is_err() {
                return harness_error(
                    &path,
                    &area,
                    "WPT's testharness.js did not load".to_owned(),
                    "harness-did-not-load",
                );
            }
            let mut engine: Box<dyn Engine> = match adapter {
                Adapter::Real => match WptHarnessEngine::new(&suite_root) {
                    Ok(engine) => Box::new(engine),
                    Err(error) => {
                        return harness_error(
                            &path,
                            &area,
                            format!("WPT's testharness.js did not load: {error}"),
                            "harness-did-not-load",
                        );
                    }
                },
                Adapter::Control => Box::new(AlwaysPasses),
            };
            let full = suite_root.join(&path);
            run_file(engine.as_mut(), &path, &area, &full, &suite_root, &source)
        });

    match spawned {
        Ok(handle) => handle.join().unwrap_or_else(|payload| {
            // The thread panicked outside the adapter call, so `run_one`'s
            // containment did not see it. Still an error, never a fail. The
            // payload is downcast because `dyn Any` is not `Display`, and its
            // content matters: a bare "Box<dyn Any>" would hide the one thing a
            // reader needs from a crash.
            let detail = payload
                .downcast_ref::<&str>()
                .map_or_else(
                    || {
                        payload
                            .downcast_ref::<String>()
                            .map_or("non-string panic payload", String::as_str)
                    },
                    |s| *s,
                )
                .to_owned();
            harness_error(
                &reported_path,
                &reported_area,
                format!("the test thread panicked: {detail}"),
                "test-thread-panicked",
            )
        }),
        Err(error) => harness_error(
            &reported_path,
            &reported_area,
            format!("could not start a test thread: {error}"),
            "test-thread-unstartable",
        ),
    }
}

fn harness_error(path: &str, area: &str, error: String, mechanism: &str) -> FileResult {
    FileResult {
        path: path.to_owned(),
        area: area.to_owned(),
        outcome: crate::outcome::Outcome::Error,
        failing_assertion: None,
        engine_location: None,
        error: Some(error),
        skip_reason: None,
        assertions_evaluated: 0,
        assertion_sites: 0,
        mechanism: Some(mechanism.to_owned()),
        notes: vec!["a harness error, not a test failure".to_owned()],
    }
}

/// Read, classify and run one test file whose source is already in hand.
fn run_file(
    engine: &mut dyn Engine,
    path: &str,
    area: &str,
    full: &Path,
    suite_root: &Path,
    source: &str,
) -> FileResult {
    // Re-derived rather than taken from the census, so the execution record
    // carries the same assertion-site count the denominator was built from. Both
    // come from one function, so they cannot drift.
    let scanned = crate::source::scan(source);
    let inline: Vec<&str> = scanned
        .scripts
        .iter()
        .filter_map(|s| s.inline.as_deref())
        .collect();
    let classification =
        crate::classify::classify(&scanned, &inline, full.parent(), Some(suite_root));

    let ctx = TestContext {
        path,
        area,
        file: full,
        suite_root,
        source,
        assertion_sites: classification.assertion_sites as u32,
    };
    let mut result = crate::engine::run_one(engine, &ctx);

    // A test the census already decided cannot run does not get to reach the
    // engine and come back as a pass. Recording the census verdict keeps the
    // four states honest.
    if classification.blocker.is_some() && result.outcome == crate::outcome::Outcome::Pass {
        result = declined(path.to_owned(), area.to_owned(), &classification, result);
    }

    // Attribute whatever happened to a mechanism, so the report can rank causes
    // rather than list files.
    result.mechanism = mechanism_for(&result, &classification);
    result
}

/// Name the mechanism behind a result, from the strongest evidence available.
///
/// The order matters and is deliberate: a real assertion failure outranks a
/// census blocker, because a test the census predicted would fail and did fail
/// is a *defect finding* rather than a capability gap, and reporting it as the
/// latter would bury it.
///
/// # Why a failure key names the *missing thing*, not the test
///
/// The first version keyed a failure on the failing assertion's text, which
/// included the test's path. That produced 62 keys for 62 tests in one directory
/// and a "ranked" table whose top entries were all one directory - a ranked list
/// of files, which is exactly what the mechanism table exists to stop being.
/// Grouping by the thing the engine lacks is what makes one entry mean one fix.
fn mechanism_for(
    result: &FileResult,
    classification: &crate::classify::Classification,
) -> Option<String> {
    if let Some(assertion) = &result.failing_assertion {
        return Some(failure_mechanism(assertion));
    }
    if let Some(blocker) = &classification.blocker {
        return Some(blocker_key(blocker));
    }
    if result.outcome == crate::outcome::Outcome::Skip {
        return result
            .notes
            .iter()
            .find(|note| note.contains("does not implement"))
            .map(|note| format!("skip: {}", mechanism::slug(note)));
    }
    None
}

/// WPT host APIs whose absence produces a failure, and the key each maps to.
///
/// Ordered as WPT writes them, longest-prefix-relevant first, because the
/// commonest real failure in the suite is "X is not defined" and the identifier
/// after it names the missing API exactly. That is a much better key than the
/// test's path: 228 tests naming `AbortSignal` are one missing interface, not
/// 228 defects.
const MISSING_HOST_APIS: &[&str] = &[
    "AbortSignal", "AbortController", "DOMException", "CustomEvent", "EventSource",
    "MessageChannel", "MessagePort", "MutationObserver", "IntersectionObserver",
    "ResizeObserver", "PerformanceObserver", "TextEncoder", "TextDecoder",
    "Notification", "Selection", "Range", "ShadowRoot", "TouchEvent",
    "PointerEvent", "ClipboardEvent", "StorageEvent", "HashChangeEvent",
    "PopStateEvent", "Storage", "Worker", "WorkerGlobalScope", "XMLHttpRequest",
    "XMLHttpRequestUpload", "FileReader", "File", "FileList", "Blob",
    "URLSearchParams", "FormData", "Headers", "Request", "Response", "WebSocket",
    "BroadcastChannel", "ReadableStream", "WritableStream", "TransformStream",
    "ImageBitmap", "ImageData", "OffscreenCanvas", "AudioContext",
    "RTCPeerConnection", "WebAssembly", "ShadowRealm", "NavigationPreloadManager",
    "IdleDetector", "PaymentRequest", "SpeechSynthesis", "VisualViewport",
    "PaymentMethod", "ScreenOrientation", "GamepadEvent",
];

/// Classify a failure by what it says is wrong.
///
/// Deliberately conservative: an unrecognised failure becomes
/// `assertion-unclassified`, which is a visible bucket saying "these need a
/// human to read", rather than a bucket with a confident-sounding name that
/// nobody checks. A mechanism table that confidently mislabels its own causes is
/// worse than one that admits a gap.
fn failure_mechanism(assertion: &str) -> String {
    // The harness prefixes the message with the test name and a colon; strip it
    // so the pattern search sees the actual failure. Only the *first* colon is
    // consumed, because a message may legitimately contain more.
    let message = assertion
        .split_once(": ")
        .map_or(assertion, |(_, rest)| rest);

    // "X is not defined" - the commonest real failure, and the clearest.
    if let Some(head) = message.split(" is not defined").next() {
        let name = head
            .rsplit(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .next()
            .unwrap_or_default();
        if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            if MISSING_HOST_APIS.contains(&name) {
                return format!("lacks-{name}");
            }
            return format!("undefined-global-{}", mechanism::slug(name));
        }
    }

    // An unimplemented method on a host object reads as a property access on
    // something with no such property.
    for (pattern, key) in [
        ("is not a function", "host-method-missing"),
        ("is not a constructor", "host-constructor-missing"),
        ("undefined is not an object", "host-object-undefined"),
        ("Cannot read propert", "host-property-missing"),
        ("is not iterable", "host-not-iterable"),
        ("Maximum call stack", "infinite-recursion"),
    ] {
        if message.contains(pattern) {
            return key.to_owned();
        }
    }

    // Order matters in this block, and getting it wrong is a real error rather
    // than a style point. `lengths differ, expected array , got ` ALSO ends with
    // "got", so the `undefined` rule below would swallow it and fold 26 genuine
    // collection defects into the IDL-default bucket - overstating the default
    // fix and hiding a different one. Most specific first.
    if message.contains("lengths differ") {
        return "value-shape-differs".to_owned();
    }

    // `expected <value> got ` with nothing after "got" is testharness.js's
    // `format_value` rendering `undefined`. 583 of the 652 shape failures are
    // this: WPT asserted a property equals `true`/`false`/`""`/a string, and the
    // engine's host object returned `undefined`.
    //
    // That is one mechanism, not 583: **host objects do not initialise the IDL
    // default values their interface declares.** `Event.cancelBubble` must
    // default to `false`, `cancelable` to `false`, and so on; returning
    // `undefined` for all of them is a single omission in one constructor rather
    // than 583 separate mistakes, and reading it as 583 would send someone to
    // fix 583 things.
    if message.trim_end().ends_with(" got") {
        return "idl-default-not-initialised".to_owned();
    }

    // Both sides present: the engine answered, and the answer was wrong. A
    // different mechanism from the missing default above, and one that must not
    // be folded into it - overstating either fix sends someone to the wrong
    // place.
    if message.contains(" got ") {
        return "wrong-value".to_owned();
    }

    if message.trim_end().ends_with(" got") {
        return "idl-default-not-initialised".to_owned();
    }
    // `expected (boolean)`, `expected (string)`, and friends: `format_value` on
    // a value whose *type* was wrong.
    if message.contains("expected (object)")
        || message.contains("expected (string)")
        || message.contains("expected (number)")
        || message.contains("expected (boolean)")
        || message.contains("expected (undefined)")
        || message.trim_end() == "expected"
    {
        return "value-shape-differs".to_owned();
    }
    if message.contains("expected a promise")
        || message.contains("Test returned a promise")
        || message.contains("must return a 'thenable' object")
        || message.contains("did not return a promise")
    {
        return "promise-shape-differs".to_owned();
    }

    // A *second* WPT harness. `resources/testdriver.js` defines `test_driver`,
    // `popup_test`, `focusAndSendDirectionalInput` and friends; a test that loads
    // it and calls one of those needs a driver this runner does not provide.
    // This is a runner gap, and it is a big one: the driver is the harness
    // WPT uses for tests that must run in a separate window or frame, which is
    // the same nested-browsing-context problem in a different file.
    for symbol in DRIVER_SYMBOLS {
        if message.contains(&format!("{symbol} is not defined")) {
            return "needs-testdriver".to_owned();
        }
    }
    // Supporting helpers a test's own `support/` file should have supplied.
    for symbol in SUPPORT_SYMBOLS {
        if message.contains(&format!("{symbol} is not defined")) {
            return "missing-support-helper".to_owned();
        }
    }

    "assertion-unclassified".to_owned()
}

/// Globals from WPT's second harness, `/resources/testdriver.js`.
///
/// A separate `needs-testdriver` key rather than a generic "missing global",
/// because the fix is not "implement this API" - it is "the runner must drive
/// `/resources/testdriver.js`, which needs nested browsing contexts". Saying so
/// is the difference between a roadmap item and a defect report.
const DRIVER_SYMBOLS: &[&str] = &[
    "test_driver", "popup_test", "runTest", "focusAndSendDirectionalInput",
    "runTestPromise", "focusAndSendKey", "getAudioURI", "wait_for_promisified_popup",
];

/// Globals that a test's own `support/` file is supposed to define.
///
/// A different key again: the missing thing is a fixture this cache did not
/// take, not an API the engine lacks. Conflating the two would send someone to
/// implement a function that WPT ships in the test's own directory.
const SUPPORT_SYMBOLS: &[&str] = &[
    "log", "opener", "origin", "promise_setup_not_supported", "onload_helpers",
];

/// A stable key for a census blocker.
///
/// `pub` because the CSS adapter attributes its results with the same keys, and
/// two adapters reporting the same cause under different names is how a
/// mechanism table ends up with the same root cause in two rows.
pub fn blocker_key(blocker: &crate::classify::Blocker) -> String {
    use crate::classify::Blocker;
    match blocker {
        Blocker::NoAssertions => "no-assertion-site".to_owned(),
        Blocker::MissingEngine(capability) => format!("lacks-{}", capability.label()),
        Blocker::UnsupportedShape(shape) => format!("shape-{}", shape.label()),
        // The URL is deliberately *not* in the key. Including it would make
        // every distinct missing fixture its own mechanism, and a table of 535
        // rows each saying "one file is missing" is the same as no table.
        Blocker::MissingFixture(_) => "missing-fixture".to_owned(),
        Blocker::Unscannable(_) => "unscannable-file".to_owned(),
    }
}

/// Turn a census-blocked adapter pass into the census's own skip verdict.
fn declined(
    path: String,
    area: String,
    classification: &crate::classify::Classification,
    previous: FileResult,
) -> FileResult {
    use crate::classify::Blocker;
    use crate::outcome::{Outcome, SkipReason};

    let (skip_reason, note) = match classification.blocker.as_ref() {
        Some(Blocker::MissingEngine(capability)) => (
            SkipReason::MissingEngineCapability,
            format!("engine lacks {}", capability.label()),
        ),
        Some(Blocker::UnsupportedShape(shape)) => (
            SkipReason::UnsupportedHarnessShape,
            format!("this runner does not implement the {} harness shape", shape.label()),
        ),
        Some(Blocker::MissingFixture(url)) => (
            SkipReason::MissingFixture,
            format!("declared fixture is absent from the checkout: {url}"),
        ),
        Some(Blocker::Unscannable(why)) => (
            SkipReason::UnverifiedCapability,
            format!("could not scan the file confidently: {why}"),
        ),
        Some(Blocker::NoAssertions) | None => (
            SkipReason::UnverifiedCapability,
            "the census found this test has no way to fail".to_owned(),
        ),
    };
    let mut notes = previous.notes;
    notes.push(format!("census verdict supersedes the adapter's pass: {note}"));
    FileResult {
        path,
        area,
        outcome: Outcome::Skip,
        failing_assertion: None,
        engine_location: None,
        error: None,
        skip_reason: Some(skip_reason),
        assertions_evaluated: previous.assertions_evaluated,
        assertion_sites: previous.assertion_sites,
        notes,
        mechanism: previous.mechanism,
    }
}

/// Walk one area, yielding suite-relative test paths in a stable order.
pub fn walk_area(root: &Path, area: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.join(area)];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|e| e == "html") {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    // Sorted: an unsorted walk makes a partial run select a different subset on
    // a different machine, which turns a labelled PARTIAL run into a different
    // suite without saying so.
    out.sort();
    out
}

#[cfg(all(test, feature = "engine"))]
mod tests {
    use super::{HarnessStatus, Harvest, severity, slug};
    use crate::mechanism::Category;
    use crate::outcome::Outcome;

    #[test]
    fn harness_statuses_map_from_the_harness_own_numbers() {
        // The five values are `Test.statuses` in testharness.js. A drift here
        // would silently turn every fail into a pass, which is the most
        // dangerous possible mapping error.
        assert_eq!(HarnessStatus::from_number(0.0), HarnessStatus::Pass);
        assert_eq!(HarnessStatus::from_number(1.0), HarnessStatus::Fail);
        assert_eq!(HarnessStatus::from_number(2.0), HarnessStatus::Timeout);
        assert_eq!(HarnessStatus::from_number(3.0), HarnessStatus::NotRun);
        assert_eq!(HarnessStatus::from_number(4.0), HarnessStatus::PreconditionFailed);
        // An unrecognised number must not land in Pass.
        assert_eq!(HarnessStatus::from_number(9.0), HarnessStatus::Unknown);
        assert_eq!(HarnessStatus::from_number(-1.0), HarnessStatus::Unknown);
    }

    #[test]
    fn a_failure_outranks_everything_that_is_not_one() {
        // The invariant that matters: nothing that is not a `Fail` may outrank
        // a `Fail` in the "worst subtest wins" rule, or a file containing one
        // real failure alongside several skips would be recorded as a skip and
        // the defect would vanish from the report entirely.
        for other in [
            HarnessStatus::Pass,
            HarnessStatus::PreconditionFailed,
            HarnessStatus::NotRun,
            HarnessStatus::Timeout,
        ] {
            assert!(
                severity(HarnessStatus::Fail) > severity(other),
                "Fail must outrank {other:?}"
            );
        }
        // A status this runner cannot interpret outranks everything, because the
        // only safe reading of an unknown verdict is "do not claim a pass".
        assert!(severity(HarnessStatus::Unknown) > severity(HarnessStatus::Fail));
    }

    #[test]
    fn an_unmet_precondition_is_better_than_a_test_that_never_ran() {
        // `NOTRUN` means the harness lost the thread - the test registered and
        // no result ever arrived. `PRECONDITION_FAILED` means the test ran,
        // checked a declared precondition, found it unmet and declined
        // cleanly. The second is a signal; the first is a gap. An earlier draft
        // of this test asserted the opposite ordering, and the implementation
        // was right: it would have let a lost thread be reported as a
        // deliberate decline, which is a false claim about the test.
        assert!(severity(HarnessStatus::NotRun) > severity(HarnessStatus::PreconditionFailed));
        assert!(severity(HarnessStatus::PreconditionFailed) > severity(HarnessStatus::Pass));
    }

    #[test]
    fn keys_are_stable_and_readable() {
        assert_eq!(slug("Cannot read properties of undefined"), "cannot-read-properties-of-undefined");
        assert_eq!(slug("  "), "other");
        assert!(slug(&"a".repeat(200)).len() <= 48);
    }

    #[test]
    fn a_harvest_round_trips_the_harness_shape() {
        // The exact JSON `RESULT_SINK` produces for one passing test. If the
        // parser and the sink ever disagree, the run reports "no verdict" for
        // every test and a coverage figure collapses to zero - which looks
        // exactly like an engine that cannot do anything.
        let json = r#"{"results":[{"name":"t","status":0,"message":null,"asserts":3}],"completion":{"status":0,"message":null,"num_tests":1}}"#;
        let harvest = Harvest::parse(json).expect("the sink's own shape must parse");
        assert_eq!(harvest.results.len(), 1);
        assert_eq!(harvest.results[0].name, "t");
        assert_eq!(harvest.results[0].status, 0.0);
        assert_eq!(harvest.results[0].asserts, 3);
        assert_eq!(harvest.completion.as_ref().map(|c| c.tests), Some(1));
    }

    #[test]
    fn a_failing_test_round_trips_with_its_message() {
        let json = r#"{"results":[{"name":"t","status":1,"message":"expected 1 but got 2","asserts":1}],"completion":null}"#;
        let harvest = Harvest::parse(json).expect("parse");
        assert_eq!(HarnessStatus::from_number(harvest.results[0].status), HarnessStatus::Fail);
        assert_eq!(
            harvest.results[0].message.as_deref(),
            Some("expected 1 but got 2")
        );
        assert!(harvest.completion.is_none());
    }

    #[test]
    fn a_brace_inside_a_failure_message_does_not_end_the_object() {
        // Real WPT failure messages contain braces - `assert_equals` prints both
        // values with `format_value`. A parser that ended the object at the
        // first `}` would read a failing test as an unreadable one, and would
        // report it as a harness limitation instead of a defect. That is the
        // exact direction that hides a real failure.
        let json = r#"{"results":[{"name":"t","status":1,"message":"expected {a: 1} but got {a: 2}","asserts":1}],"completion":null}"#;
        let harvest = Harvest::parse(json).expect("a message with braces must parse");
        assert_eq!(harvest.results.len(), 1);
        assert_eq!(
            harvest.results[0].message.as_deref(),
            Some("expected {a: 1} but got {a: 2}")
        );
    }

    #[test]
    fn an_empty_result_array_parses_to_no_results() {
        let harvest = Harvest::parse(r#"{"results":[],"completion":null}"#).expect("parse");
        assert!(harvest.results.is_empty());
        assert!(harvest.completion.is_none());
    }

    #[test]
    fn unparseable_input_is_refused_rather_than_guessed() {
        // The negative control for the reader. Every one of these must be
        // `None`, which the caller turns into an explained skip. Guessing a
        // status from malformed input is how a broken reader reports a
        // confident pass rate.
        for input in ["", "not json", "{}", "{\"results\":", "[1,2,3]"] {
            assert!(Harvest::parse(input).is_none(), "{input:?} must not parse");
        }
    }

    #[test]
    fn a_missing_host_api_is_keyed_by_the_api_not_the_test() {
        // The reason a failure key must not include the test path: 228 tests
        // naming `AbortSignal` are one missing interface, not 228 defects. A key
        // carrying the path produced 62 keys for one directory and a "ranked"
        // table of files, which is what this table exists to stop being.
        for name in ["AbortSignal", "MutationObserver", "ResizeObserver", "Range"] {
            let key = super::failure_mechanism(&format!(
                "dom/x/y-crash.html: {name} is not defined (the harness reported 0 test(s))"
            ));
            assert_eq!(key, format!("lacks-{name}"));
        }
    }

    #[test]
    fn a_shape_mismatch_is_keyed_by_the_family_not_the_message() {
        // Grouping by message text would give one key per distinct expected
        // value, and the "ranked" table would rank WPT's fixtures.
        assert_eq!(
            super::failure_mechanism("t: lengths differ, expected array , got "),
            "value-shape-differs"
        );
        assert_eq!(
            super::failure_mechanism("t: Loose id: expected (object) "),
            "value-shape-differs"
        );
        assert_eq!(
            super::failure_mechanism("t: TypeError: f is not a function"),
            "host-method-missing"
        );
    }

    #[test]
    fn an_undefined_result_is_one_mechanism_not_one_per_property() {
        // 583 of the 652 shape failures were this. `Event.cancelBubble` defaulting
        // to `undefined` instead of `false` and `initEvent` properties behaving
        // the same way are one omission in how host objects are constructed, not
        // 583 separate defects. Splitting them would send someone to fix 583
        // things instead of one.
        for message in [
            "cancelBubble must be false when an event is initially created.: expected false got ",
            "Default prevention via preventDefault: expected true got ",
        ] {
            assert_eq!(
                super::failure_mechanism(&format!("t: {message}")),
                "idl-default-not-initialised",
                "{message:?} returns undefined, so it is a missing default"
            );
        }
        assert_eq!(
            super::failure_mechanism("t: basic with click(): expected true got "),
            "idl-default-not-initialised"
        );
    }

    #[test]
    fn wpts_own_type_mismatch_messages_are_one_family() {
        // testharness.js's `format_value` prints `expected (string)`, and a bare
        // `expected` when the whole message was the prefix. Leaving these out
        // put 488 real failures into the unattributed bucket, which is how a
        // ranked table stops being actionable: the biggest bucket was "we do not
        // know", not a thing anyone can fix.
        for message in [
            "expected (string)",
            "expected (number)",
            "expected (object) ",
            "lengths differ, expected array , got ",
        ] {
            assert_eq!(
                super::failure_mechanism(&format!("t: {message}")),
                "value-shape-differs",
                "{message:?} is a type/shape mismatch"
            );
        }
    }

    #[test]
    fn a_wrong_value_is_a_different_mechanism_from_a_missing_one() {
        // Both sides present means the engine answered and the answer was wrong.
        // Folding this into the IDL-default family would overstate the default-
        // initialisation fix by however many of these there are, and the two
        // need different work.
        assert_eq!(
            super::failure_mechanism("t: expected true got false"),
            "wrong-value"
        );
        // Only one side present means `undefined`, which is the missing default.
        assert_eq!(
            super::failure_mechanism("t: expected false got "),
            "idl-default-not-initialised"
        );
    }

    #[test]
    fn the_second_wpt_harness_gets_its_own_key() {
        // `test_driver` comes from `/resources/testdriver.js`, and the fix is
        // "drive WPT's testdriver", not "implement test_driver". A generic
        // missing-global bucket would report it as an unimplemented API and put
        // a phantom roadmap item in front of a reader.
        assert_eq!(
            super::failure_mechanism("t: test_driver is not defined"),
            "needs-testdriver"
        );
        assert_eq!(
            super::failure_mechanism("t: focusAndSendDirectionalInput is not defined"),
            "needs-testdriver"
        );
    }

    #[test]
    fn a_missing_support_helper_is_a_fixture_gap_not_an_api_gap() {
        // These live in the test's own `support/` directory. Attributing them to
        // the engine would send someone to implement a function WPT already ships.
        assert_eq!(
            super::failure_mechanism("t: log is not defined"),
            "missing-support-helper"
        );
        assert_eq!(
            super::failure_mechanism("t: opener is not defined"),
            "missing-support-helper"
        );
    }

    #[test]
    fn an_unattributable_failure_is_its_own_visible_bucket() {
        // Not folded into a confident-sounding name. A bucket that says "these
        // need a human" is honest; one that guesses is a defect list nobody
        // checks.
        assert_eq!(
            super::failure_mechanism("t: some entirely novel failure mode"),
            "assertion-unclassified"
        );
        // And a lowercase name is *not* routed to the host-API path. WPT's
        // platform globals are all capitalised (`Range`, `Worker`), so a
        // lowercase undefined name is a bug in the test's own script - an
        // undeclared variable, a scoping mistake - and attributing it to a
        // missing engine interface would be a defect report about the engine
        // for something the engine did nothing wrong in. It lands in the
        // unattributed bucket, which is where a human should look.
        assert_eq!(
            super::failure_mechanism("t: foo is not defined"),
            "assertion-unclassified"
        );
    }

    #[test]
    fn an_unrecognised_capitalised_global_is_named_rather_than_bucketed() {
        // A capitalised name is a platform global by WPT's own convention, so
        // naming it is more useful than a generic bucket - even when this
        // runner does not know the interface.
        let key = super::failure_mechanism("t: FooPlatformGlobal is not defined");
        assert!(
            key.starts_with("undefined-global-"),
            "{key} should name the missing global"
        );
    }

    #[test]
    fn the_recursion_guard_is_not_reported_as_an_engine_defect() {
        // The engine's own limit fired, which is the limit working. Counting it
        // as a defect would put a row in the defect list for behaviour the
        // engine chose on purpose.
        assert_eq!(
            super::failure_mechanism("t: RangeError: Maximum call stack size exceeded"),
            "infinite-recursion"
        );
    }

    #[test]
    fn a_pass_is_not_a_defect_and_a_fail_is() {
        // The four-category mapping, stated as a test. A missing API and a wrong
        // answer are different claims and a defect list that contains the
        // first is a false defect report.
        assert!(!Category::UnimplementedFeature.is_a_defect());
        assert!(!Category::HarnessLimitation.is_a_defect());
        assert!(!Category::AcceptableDifference.is_a_defect());
        assert!(Category::EngineDefect.is_a_defect());
        assert_ne!(Outcome::Pass, Outcome::Fail);
    }
}
