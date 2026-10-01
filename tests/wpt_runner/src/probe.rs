//! Can the engine actually load WPT's own harness?
//!
//! This is the question the whole round turns on, and it is worth answering
//! with a measurement rather than an argument. `dom/` and `html/` tests call
//! `test()`, `promise_test()` and `async_test()` from
//! `/resources/testharness.js`, which is 198 KB and 4,725 lines of ordinary
//! modern JavaScript. If `render-js` cannot parse and run that file, the two
//! areas cannot be executed no matter how good the adapter is, and the honest
//! answer is "no, and here is the specific mechanism".
//!
//! If it can, then the areas *are* executable and the adapter is the only
//! thing between this runner and a measured number.
//!
//! So the probe measures, in order:
//!
//! 1. does `testharness.js` **compile** (parse) at all;
//! 2. does it **execute** without throwing;
//! 3. does it **define** the globals a test needs (`test`, `assert_equals`,
//!    `promise_test`, `async_test`, `setup`);
//! 4. does a real `dom/` test then run through it, and what does the harness
//!    say.
//!
//! Each step is reported separately, because "no" is only useful if it says
//! *which* of the four failed and why. A harness that reports a single
//! "unsupported" for all of them tells a reader nothing.

use std::path::{Path, PathBuf};

/// One step of the probe, and what it observed.
#[derive(Clone, Debug)]
pub struct ProbeStep {
    pub name: String,
    pub held: bool,
    pub detail: String,
}

/// The whole probe: does the engine's JavaScript runtime reach WPT's harness?
#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub steps: Vec<ProbeStep>,
    /// Suite revision the probe ran against, so a result is attributable.
    pub suite_revision: String,
    /// Whether the four globals a test needs were all defined.
    pub harness_usable: bool,
}

impl Probe {
    pub fn record(&mut self, name: &str, held: bool, detail: String) {
        self.steps.push(ProbeStep {
            name: name.to_owned(),
            held,
            detail,
        });
    }

    /// The verdict a reader acts on: the first step that failed, and its detail.
    #[must_use]
    pub fn first_failure(&self) -> Option<&ProbeStep> {
        self.steps.iter().find(|s| !s.held)
    }

    #[must_use]
    pub fn all_held(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(|s| s.held)
    }
}

/// The harness globals a WPT test cannot be written without.
///
/// Checked by *name presence in the realm after execution*, not by reading
/// testharness.js. The reason is that reading the file would prove the file
/// declares them, and the question is whether the *engine* ends up with them -
/// a script that parses, throws halfway, and leaves the realm half-built is a
/// real failure mode and only the realm shows it.
pub const REQUIRED_GLOBALS: &[&str] = &[
    "test",
    "promise_test",
    "async_test",
    "setup",
    "assert_equals",
    "assert_true",
    "assert_array_equals",
    "assert_throws_js",
];

/// The legacy `setup()` harness, which the last round recorded as unsupported.
///
/// Measured here rather than asserted, because "unsupported" was a claim about
/// the pinned suite made from memory and it was wrong: `testharness.js` calls
/// `expose(setup, 'setup')` at line 1294 and runs the function synchronously
/// before the tests. Getting this wrong cost 772 tests out of the executable
/// population and mislabelled a working feature as a gap, which is the exact
/// class of error this crate exists to prevent - just pointed the other way.
pub const SETUP_GLOBAL: &str = "setup";

/// Read a file, or return a step-shaped error string.
#[cfg(feature = "engine")]
fn read(suite_root: &Path, relative: &str) -> Result<String, String> {
    std::fs::read_to_string(suite_root.join(relative))
        .map_err(|e| format!("could not read {relative}: {e}"))
}

/// Run the probe against a pinned checkout.
///
/// `#[cfg]`-gated on the `engine` feature in the caller: without it there is
/// no runtime to probe, and a probe that reports "no" because it had nothing to
/// probe with is exactly the kind of number this crate refuses to produce.
#[cfg(feature = "engine")]
pub fn run(suite_root: &Path, revision: &str) -> Probe {
    use render_core::html::parse_document;
    use render_core::js::{JsRuntime, RuntimeLimits};

    let mut probe = Probe {
        suite_revision: revision.to_owned(),
        ..Probe::default()
    };

    // ---- Step 1: the harness file exists at the pinned revision ------------
    let harness = match read(suite_root, "resources/testharness.js") {
        Ok(text) => {
            probe.record(
                "resources/testharness.js is present at the pinned revision",
                true,
                format!("{} bytes", text.len()),
            );
            text
        }
        Err(detail) => {
            probe.record(
                "resources/testharness.js is present at the pinned revision",
                false,
                detail,
            );
            return probe;
        }
    };

    // ---- Step 2: it compiles (parses) under the engine's own limits --------
    //
    // The *default* runtime limits are used deliberately. Raising them to make
    // the harness fit would measure a configuration no WPT test will run in,
    // and a pass rate from an artificially enlarged budget is a number about
    // the budget rather than the engine.
    let limits = RuntimeLimits::default();
    let compiled = render_core::js::CompiledScript::compile(&harness, &limits);
    let compiled = match compiled {
        Ok(script) => {
            probe.record(
                "the engine's parser compiles the whole harness at default limits",
                true,
                "compiled".to_owned(),
            );
            script
        }
        Err(error) => {
            probe.record(
                "the engine's parser compiles the whole harness at default limits",
                false,
                format!("{error}"),
            );
            return probe;
        }
    };

    // ---- Step 3: it executes and defines the globals a test needs ----------
    //
    // A fresh document per attempt, because a runtime bound to a document the
    // engine has already mutated is a different experiment from a clean load.
    let mut parsed = parse_document("<!doctype html><title>probe</title><body></body>");
    let mut runtime = JsRuntime::with_limits(&parsed.dom, limits);

    let outcome = runtime.execute_compiled(&mut parsed.dom, &compiled);
    let mut execute_error = None;
    if let Err(error) = outcome {
        execute_error = Some(format!("{error}"));
    }
    // Whether it threw is one thing; whether the realm is usable is the
    // question that matters, and a harness that throws *and* still defined
    // `test` would be more useful than one that threw and defined nothing. So
    // both are measured and neither is inferred from the other.
    let defined: Vec<&str> = REQUIRED_GLOBALS
        .iter()
        .copied()
        .filter(|name| !matches!(runtime.realm().global(name), None | Some(render_core::js::JsValue::Undefined)))
        .collect();
    let missing: Vec<&str> = REQUIRED_GLOBALS
        .iter()
        .copied()
        .filter(|name| !defined.contains(name))
        .collect();

    let threw_but_usable = execute_error.is_some() && missing.is_empty();
    if threw_but_usable {
        probe.record(
            "the harness executes and defines every global a WPT test needs",
            true,
            format!(
                "execution reported {} but all {} globals are defined",
                execute_error.as_deref().unwrap_or(""),
                REQUIRED_GLOBALS.len()
            ),
        );
    } else if !missing.is_empty() {
        // The failure is the missing globals, and the detail says so whether or
        // not execution also threw. Both facts are reported, because they point
        // at different fixes: a parse or execute error is an engine defect in
        // the JavaScript runtime, and a missing global is a harness that has
        // not finished loading.
        probe.record(
            "the harness executes and defines every global a WPT test needs",
            false,
            format!(
                "{}/{} globals defined; missing: {}{}",
                defined.len(),
                REQUIRED_GLOBALS.len(),
                missing.join(", "),
                execute_error
                    .as_ref()
                    .map_or(String::new(), |e| format!("; execution also failed: {e}"))
            ),
        );
        return probe;
    } else if let Some(error) = &execute_error {
        probe.record(
            "the harness executes and defines every global a WPT test needs",
            false,
            format!("execution failed: {error}; all {} globals are defined", REQUIRED_GLOBALS.len()),
        );
        return probe;
    } else {
        // All globals defined and nothing threw. This branch is reached only by
        // a completely clean load, and the earlier draft of this probe reported
        // it as a failure with the self-contradictory detail "8/8 globals
        // defined; missing:" - a number that was impossible on its face and that
        // only reading the output caught. The condition is now written so the
        // clean case cannot reach a failure branch.
        probe.record(
            "the harness executes and defines every global a WPT test needs",
            true,
            format!("all {} globals defined, no error", REQUIRED_GLOBALS.len()),
        );
    }

    probe.harness_usable = missing.is_empty();

    // ---- Step 4: which DOM primitives the harness needs are present -------
    //
    // The harness's own failure mode is a missing `document` method surfacing
    // as `Cannot read properties of undefined` several thousand lines later,
    // with the real cause named nowhere. Measuring the primitives directly
    // turns one opaque error into a list of named gaps, and that list is the
    // difference between "the harness does not run" and "the engine is missing
    // createElementNS".
    //
    // Each expression is a *capability question*, and each is reported
    // separately rather than folded into one "does DOM work" line: a harness
    // that reported only the aggregate would hide which single missing method
    // stopped 30,000 tests.
    for capability in DOM_CAPABILITIES {
        let expression = format!("typeof {}", capability.expression);
        let observed = runtime
            .execute(&mut parsed.dom, &expression)
            .map(|outcome| outcome.value.to_js_string())
            .unwrap_or_else(|error| format!("<threw: {error}>"));
        probe.record(
            &format!("document.{} is callable", capability.method),
            observed == "function",
            format!("typeof {} evaluated to {observed}", capability.expression),
        );
    }

    // ---- Step 5: a real test runs through the loaded harness --------------
    //
    // Deliberately **not** gated on the `createElementNS` step above. That step
    // is expected to fail on this engine until it grows the method, and an
    // earlier version returned at the first failure - so the two most important
    // steps never ran and the probe reported "10 of 13" while having tested
    // nothing about whether a test can execute. A probe that stops at the first
    // problem cannot distinguish "this is the only problem" from "this is the
    // first of several", and the second reading is the actionable one.
    //
    // The two cases are pass and fail, and both are required. A harness that
    // reports the same verdict for a satisfied and an unsatisfied assertion has
    // proved nothing, and "pass" is also the value a harness initialises to and
    // never updates - so only the falsifying case can distinguish them.
    let satisfied = probe_trivial_test(&mut runtime, &mut parsed.dom, "assert_equals(1, 1)");
    let satisfied_status = probe_trivial_test_status(&runtime);
    probe.record(
        "a test with a satisfied assertion is reported PASS by the loaded harness",
        satisfied.is_ok() && satisfied_status.as_deref() == Some("0"),
        format!(
            "run: {}, tests.status.status: {}",
            satisfied.as_ref().err().map_or("ok", String::as_str),
            satisfied_status.as_deref().unwrap_or("unreadable")
        ),
    );

    // A fresh realm for the falsifying case: the harness's `Tests` object is a
    // singleton, and reusing it would let the first test's status leak into the
    // second one's reading. A probe that cannot falsify is a probe that cannot
    // be trusted.
    let mut falsifying_parsed =
        parse_document("<!doctype html><title>probe</title><body></body>");
    let mut falsifying_runtime = JsRuntime::with_limits(&falsifying_parsed.dom, limits);
    let _ = falsifying_runtime.execute_compiled(&mut falsifying_parsed.dom, &compiled);
    let falsifying = probe_trivial_test(
        &mut falsifying_runtime,
        &mut falsifying_parsed.dom,
        "assert_equals(1, 2)",
    );
    let falsifying_status = probe_trivial_test_status(&falsifying_runtime);
    // The falsification is that the harness *distinguishes*: PASS for the
    // satisfied assertion and something else for the unsatisfied one. Both
    // halves are checked, because "everything reports 0" and "everything
    // reports 1" are each a way for this probe to lie.
    let falsified = falsifying.is_ok()
        && falsifying_status.is_some()
        && falsifying_status != satisfied_status;
    probe.record(
        "a test with a FALSE assertion is reported NOT-pass by the same harness",
        falsified,
        format!(
            "satisfied={} vs falsified={}; the harness distinguishes them: {}",
            satisfied_status.as_deref().unwrap_or("unreadable"),
            falsifying_status.as_deref().unwrap_or("unreadable"),
            if falsified { "yes" } else { "no" }
        ),
    );

    probe
}

/// One `document` method the WPT harness calls, and the expression that
/// measures whether it is callable.
///
/// Kept as data rather than as code so the report names the missing method
/// rather than reporting a count, and so adding a method is a one-line change
/// that cannot accidentally change how the others are measured.
#[cfg(feature = "engine")]
const DOM_CAPABILITIES: &[Capability_] = &[
    Capability_ { method: "createElement", expression: "document.createElement" },
    Capability_ { method: "createElementNS", expression: "document.createElementNS" },
    Capability_ { method: "createTextNode", expression: "document.createTextNode" },
    Capability_ { method: "getElementById", expression: "document.getElementById" },
    Capability_ { method: "getElementsByTagName", expression: "document.getElementsByTagName" },
    Capability_ { method: "querySelector", expression: "document.querySelector" },
    Capability_ { method: "querySelectorAll", expression: "document.querySelectorAll" },
    Capability_ { method: "addEventListener", expression: "document.addEventListener" },
];

#[cfg(feature = "engine")]
struct Capability_ {
    method: &'static str,
    expression: &'static str,
}

/// Run one real test through a loaded harness and read its verdict back.
///
/// `body` is the assertion the test makes, injected as a parameter rather than
/// baked in, because the same function has to be callable with a satisfied and
/// with a falsified assertion. That is the whole point of it: a helper that can
/// only run the passing case is not evidence of anything.
#[cfg(feature = "engine")]
fn probe_trivial_test(
    runtime: &mut render_core::js::JsRuntime,
    dom: &mut render_core::dom::Dom,
    body: &str,
) -> Result<(), String> {
    let script = format!("test(function() {{ {body} }}, 'probe trivial');");
    runtime.execute(dom, &script).map(|_| ()).map_err(|e| format!("{e}"))
}

/// Read `tests.status.status` out of the realm, if the harness installed it.
///
/// Uses only the public surface: [`render_core::js::Realm::global`] to reach
/// `tests`, and [`render_core::js::Realm::object`] plus
/// [`render_core::js::JsObject::own_property`] to walk down. A probe that
/// needed a private accessor would be a probe that could not be trusted, and
/// worse, one whose failure would look like an engine failure.
#[cfg(feature = "engine")]
fn probe_trivial_test_status(runtime: &render_core::js::JsRuntime) -> Option<String> {
    use render_core::js::{JsValue, ObjectId};

    fn own(realm: &render_core::js::Realm, object: ObjectId, key: &str) -> Option<JsValue> {
        realm
            .object(object)?
            .own_property(key)
            .map(|descriptor| descriptor.value.clone())
    }

    let tests = match runtime.realm().global("tests") {
        Some(JsValue::Object(object)) => object,
        Some(other) => return Some(format!("`tests` is {other:?}, not an object")),
        None => return Some("`tests` is not a global".to_owned()),
    };
    let status = match own(runtime.realm(), tests, "status") {
        Some(JsValue::Object(object)) => object,
        Some(other) => return Some(format!("`tests.status` is {other:?}, not an object")),
        None => return Some("`tests` has no `status` property".to_owned()),
    };
    match own(runtime.realm(), status, "status") {
        Some(JsValue::Number(number)) => Some(format!("{number}")),
        Some(other) => Some(format!("`tests.status.status` is {other:?}, not a number")),
        None => Some("`tests.status` has no `status` property".to_owned()),
    }
}

/// Locate the checkout, for callers that do not already have it.
#[must_use]
pub fn suite_path(root: &Path) -> PathBuf {
    root.join("tools/wpt/.cache/wpt")
}
#[cfg(test)]
mod tests {
    use super::{Probe, REQUIRED_GLOBALS};

    #[test]
    fn a_probe_that_ran_reports_its_first_failure_in_order() {
        let mut probe = Probe::default();
        probe.record("a", true, "ok".to_owned());
        probe.record("b", false, "nope".to_owned());
        probe.record("c", false, "later".to_owned());
        let first = probe.first_failure().expect("a failure");
        assert_eq!(first.name, "b");
        assert!(!probe.all_held());
    }

    #[test]
    fn an_empty_probe_is_not_a_pass() {
        // The exact shape of a false "the engine is fine": zero steps, all of
        // them trivially satisfied. `all_held` refuses it.
        let probe = Probe::default();
        assert!(!probe.all_held());
        assert!(probe.first_failure().is_none());
    }

    #[test]
    fn the_required_globals_are_the_ones_a_wpt_test_calls() {
        // If this list drifts from what tests actually call, the probe stops
        // meaning anything, and it would still pass.
        for name in ["test", "promise_test", "async_test", "assert_equals", "assert_true"] {
            assert!(REQUIRED_GLOBALS.contains(&name), "{name} is required by the suite");
        }
    }
}
