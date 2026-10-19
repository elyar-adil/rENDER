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

use render_core::document::Document;
use render_core::script::{
    ScriptDiscoveryLimits, ScriptScheduling, ScriptSource, discover_scripts,
};
use render_core::js::{
    CompiledScript, DocumentReadyState, JsRuntime, RuntimeLimits, TimerRequest,
};

use crate::classify::Capability;
use crate::engine::{Engine, HarnessError, SubtestEvidence, TestContext, Verdict};
use crate::mechanism::{self, Category};
use crate::reftest::percent_decode;
use crate::results::FileResult;

/// What this adapter is, stated in the results file.
pub const ENGINE_DESCRIPTION: &str = concat!(
    "render-core + render-js: WPT testharness.js executed unmodified in the engine's own ",
    "realm, results read through add_result_callback"
);

/// Maximum microtask batches and timer callbacks drained per test.
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
    /// The harness's failure message, when it gave one.
    pub message: Option<String>,
    /// Assertions the runner's counter recorded across the file. It is the total
    /// of the per-subtest counts in [`Execution::subtests`]. See [`RESULT_SINK`].
    pub assertions: u32,
    /// What the harness reported about the file's subtests. `None` when the
    /// execution never reached the harness's results.
    pub subtests: Option<SubtestEvidence>,
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
    /// The pinned checkout. Nested browsing contexts load their documents and
    /// scripts from it, through [`SuiteLoader`].
    suite_root: std::path::PathBuf,
}

/// The host that WPT's own test server serves the suite from. The suite is
/// read from disk instead, so a test's URLs resolve against this host and map
/// onto the checkout.
pub(crate) const WPT_HOST: &str = crate::reftest::SUITE_HOST;

/// The URL a suite test runs at: its path under the suite's own host.
pub(crate) fn test_url(path: &str) -> url::Url {
    url::Url::parse(&format!("http://{WPT_HOST}/{path}"))
        .unwrap_or_else(|_| url::Url::parse("about:blank").expect("constant URL"))
}

/// Serves the pinned checkout to nested browsing contexts: a URL under the
/// suite's host names a file under the checkout, and every other URL is
/// unavailable.
#[derive(Debug)]
pub(crate) struct SuiteLoader {
    pub(crate) root: std::path::PathBuf,
}

impl render_core::js::DocumentLoader for SuiteLoader {
    fn load_text(&mut self, url: &url::Url) -> Option<String> {
        if url.host_str() != Some(WPT_HOST) {
            return None;
        }
        let decoded = percent_decode(url.path());
        let relative = decoded.trim_start_matches('/');
        if relative.split('/').any(|part| part == "..") {
            return None;
        }
        // A `.sub` template or a `.py` handler is a response the WPT server generates.
        // Its file is source text, not the document a browser would load, so it is refused
        // rather than parsed as a page.
        if relative.contains("{{") || is_server_generated(relative) {
            return None;
        }
        std::fs::read_to_string(self.root.join(relative)).ok()
    }
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
            suite_root: suite_root.to_path_buf(),
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
        // Reference tests and crash tests are scored by pixels and by completing, not by
        // testharness.js, so they never reach the harness below. A file that is neither
        // returns `None` and takes the testharness path unchanged.
        if let Some(verdict) = crate::wptreftest::score(ctx, &self.suite_root) {
            return verdict;
        }

        // Refuse before doing any work. A test that needs a nested browsing
        // context cannot be driven by anything this adapter does, and a test
        // that drives an iframe would otherwise half-run and report a verdict
        // about the wrong document.
        if let Some(capability) = first_unsupported_capability(ctx) {
            return Ok(Verdict::Unsupported {
                capability: Some(capability),
                reason: format!(
                    "the engine lacks {}, so the test cannot be driven",
                    capability.label()
                ),
                subtests: None,
            });
        }

        // The test's own document, and the classic scripts it declares, resolved
        // before anything runs. A declared script this runner cannot serve is decided
        // here, so no test runs half way and then fails on a helper it never had.
        let mut document = Document::parse(ctx.source);
        let plan = match plan_scripts(&document, &test_url(ctx.path), &self.suite_root) {
            Ok(plan) => plan,
            Err(refusal) => return Ok(refusal.into_verdict()),
        };
        let execution = self.execute_test(ctx, &mut document, &plan)?;
        Ok(with_notes(execution.into_verdict(ctx), plan.notes()))
    }
}

impl WptHarnessEngine {
    /// Run one test and report what the harness said.
    fn execute_test(
        &mut self,
        ctx: &TestContext<'_>,
        document: &mut Document,
        plan: &ScriptPlan,
    ) -> Result<Execution, HarnessError> {
        // A fresh document per test, from the test's own HTML. Sharing one
        // document between tests would let state leak and turn an engine
        // ordering bug into a hundred apparent failures.
        let mut runtime =
            JsRuntime::with_limits_and_url(document.dom(), self.limits, &test_url(ctx.path));
        runtime.set_document_loader(Box::new(SuiteLoader {
            root: self.suite_root.clone(),
        }));
        // The 2D canvas surface is an embedder service, installed as `Page`
        // installs it. Without a host, `getContext` answers null, and every
        // canvas test would measure the missing host rather than the engine.
        runtime.install_canvas_host(Box::new(render_core::canvas::CanvasHandle::default()));

        // 1. Load WPT's harness, unmodified, from the pinned checkout.
        runtime
            .execute_compiled(document.dom_mut(), &self.harness)
            .map_err(|error| HarnessError {
                message: format!("testharness.js did not execute: {error}"),
            })?;

        // 2. Register the result channel *before* the test runs. The harness
        //    exposes `add_result_callback` precisely so an out-of-process driver
        //    can do this, and registering afterwards would miss every result
        //    from a synchronously-run test.
        runtime
            .execute(
                document.dom_mut(),
                RESULT_SINK,
            )
            .map_err(|error| HarnessError {
                message: format!("could not register the result callback: {error}"),
            })?;

        // 3. Run the classic scripts the test declares, in the order `plan_scripts`
        //    resolved them. A script that throws reports its error to the window and
        //    the rest still run, as HTML's "report the exception" requires.
        for script in &plan.scripts {
            let outcome = match &script.url {
                Some(url) => runtime.execute_with_filename(document.dom_mut(), &script.source, url),
                None => runtime.execute(document.dom_mut(), &script.source),
            };
            if let Err(error) = outcome {
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
                        subtests: None,
                        mechanism: Some("engine-step-budget".to_owned()),
                        category: Category::HarnessLimitation,
                    });
                }
            }
        }

        // 3b. The document lifecycle a browser runs once the page's scripts have run
        //     (HTML "the end"). The page's own `load` event is fired through
        //     `dispatch_page_load`, the one path that fires it; the steps before it
        //     are the readiness changes and their events. testharness.js completes a
        //     file only after `load`, and a test waiting for any of these events
        //     never starts without them.
        if let Err(error) = run_page_lifecycle(&mut runtime, document.dom_mut())
            && error.kind() == render_core::js::JsErrorKind::ResourceLimit
        {
            return Ok(Execution {
                status: HarnessStatus::NotRun,
                name: ctx.path.to_owned(),
                message: Some(format!("engine step budget exhausted: {error}")),
                assertions: 0,
                subtests: None,
                mechanism: Some("engine-step-budget".to_owned()),
                category: Category::HarnessLimitation,
            });
        }

        // 4. Drain the queues. `promise_test` and `async_test` finish in
        //    microtasks and timers, and stopping before they run would report
        //    NOTRUN for every async test - a systematic under-count that looks
        //    like an engine that cannot do async at all.
        let drained = Self::drain(&mut runtime, document.dom_mut());
        if !drained.settled {
            return Ok(Execution {
                status: HarnessStatus::NotRun,
                name: ctx.path.to_owned(),
                message: Some(format!(
                    "the event loop did not settle within {MAX_DRAIN_TURNS} turns"
                )),
                assertions: 0,
                subtests: None,
                mechanism: Some("event-loop-never-settles".to_owned()),
                category: Category::HarnessLimitation,
            });
        }

        Ok(read_results(&mut runtime, document.dom_mut(), ctx.path))
    }

    /// Drain microtasks and timers to a fixed bound, in HTML timer order.
    ///
    /// Timers fire in order of their due time on a virtual clock: a timer is due
    /// at the clock value when it was requested plus its delay, and the clock
    /// advances to the due time of each timer that fires (HTML 8.6, timer
    /// initialization steps). Firing in request order instead lets a
    /// testharness.js global timeout requested before a test's own
    /// `step_timeout` run first and end the test as TIMEOUT.
    ///
    /// Microtasks are run to completion before each timer, as a microtask
    /// checkpoint does. Intervals are re-armed from the period
    /// [`JsRuntime::fire_timer_reporting`] returns, so a `clearInterval` inside
    /// the callback stops them.
    ///
    /// Returns whether the loop settled. A loop that never settles is a harness
    /// limitation, never a fail: the test did not assert anything wrong, it
    /// simply never finished, and those are different claims.
    pub(crate) fn drain(runtime: &mut JsRuntime, dom: &mut render_core::dom::Dom) -> DrainOutcome {
        let mut clock = 0.0_f64;
        // The first exception a timer callback threw. The runtime has already reported it to
        // `window.onerror`; it is kept here for the reference and crash scorers, which must not
        // treat a page whose timer threw as the page they would have compared.
        let mut uncaught: Option<render_core::js::JsError> = None;
        let mut sequence = 0_u64;
        let mut pending: Vec<PendingTimer> = Vec::new();
        for _ in 0..MAX_DRAIN_TURNS {
            // A batch of microtasks is one step. Its timer requests are absorbed
            // only once no microtask is left, so a request made inside the
            // checkpoint is ordered with the rest.
            let batch = runtime.take_pending_microtasks();
            if !batch.is_empty() {
                for microtask in batch {
                    // A rejection inside a microtask is the test's business; the
                    // harness's `unhandledrejection` handler records it. Swallowing
                    // it here is correct precisely because something downstream
                    // does account for it.
                    let _ = runtime.invoke_microtask(dom, microtask);
                }
                continue;
            }

            for request in runtime.take_pending_timer_requests() {
                match request {
                    TimerRequest::Schedule { id, delay_ms } => {
                        pending.push(PendingTimer {
                            due: clock + delay_ms,
                            sequence,
                            id,
                        });
                        sequence += 1;
                    }
                    TimerRequest::Cancel { id } => pending.retain(|timer| timer.id != id),
                }
            }

            let Some(next) = pending
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| a.due.total_cmp(&b.due).then(a.sequence.cmp(&b.sequence)))
                .map(|(index, _)| index)
            else {
                return DrainOutcome {
                    settled: true,
                    uncaught,
                };
            };
            let timer = pending.swap_remove(next);
            clock = clock.max(timer.due);
            let (period, outcome) = runtime.fire_timer_reporting(dom, timer.id);
            if let Err(error) = outcome {
                uncaught.get_or_insert(error);
            }
            if let Some(period) = period {
                pending.push(PendingTimer {
                    due: clock + period,
                    sequence,
                    id: timer.id,
                });
                sequence += 1;
            }
        }
        DrainOutcome {
            settled: false,
            uncaught,
        }
    }
}

/// The suite-relative path of the harness. The adapter loads it before the test
/// runs, so a test's own `<script src>` of it is not run a second time.
const HARNESS_PATH: &str = "resources/testharness.js";

/// The classic scripts a test declares, in the order they run, with every
/// fixture resolved. See [`plan_scripts`].
#[derive(Debug, Default)]
pub(crate) struct ScriptPlan {
    /// Parser-blocking scripts in document order, then `async`, then `defer`.
    /// That is the order `render-core`'s page queue uses, so the runner and the
    /// page agree on it.
    pub(crate) scripts: Vec<RunScript>,
    /// Module scripts the test declares and this runner does not execute.
    pub(crate) modules_not_run: u32,
}

impl ScriptPlan {
    /// What the plan left out, for the result's notes, so a pass that did not run
    /// a declared module is labelled as such.
    fn notes(&self) -> Vec<String> {
        if self.modules_not_run == 0 {
            return Vec::new();
        }
        vec![format!(
            "{} module script(s) declared and not executed: this runner does not run ES \
             module scripts",
            self.modules_not_run
        )]
    }
}

/// One classic script, ready to run.
#[derive(Debug)]
pub(crate) struct RunScript {
    /// The URL a file-backed script was read from. Its uncaught errors report this
    /// as their `filename`. `None` for an inline script, whose errors report the
    /// document.
    pub(crate) url: Option<String>,
    pub(crate) source: String,
}

/// A declared script the runner cannot run, and why.
#[derive(Debug)]
pub(crate) struct Declined {
    pub(crate) url: String,
    pub(crate) mechanism: &'static str,
    pub(crate) reason: String,
}

impl Declined {
    fn into_verdict(self) -> Verdict {
        Verdict::MissingFixture {
            url: self.url,
            mechanism: self.mechanism.to_owned(),
            reason: self.reason,
        }
    }
}

fn fixture_declined(url: &url::Url, mechanism: &'static str, reason: String) -> Declined {
    Declined {
        url: url.as_str().to_owned(),
        mechanism,
        reason,
    }
}

/// Resolve the classic scripts a test declares, in the order a browser runs them.
///
/// The declaration order and the scheduling come from `render-core`'s
/// [`discover_scripts`], which is the page's own script discovery, so this file
/// does not restate HTML's `<script>` rules. What this adds is the fixture half:
/// each `src` is resolved against the test's URL and read from the checkout.
///
/// * A parser-blocking script runs where it appears, so an external one runs before
///   the inline scripts that follow it (HTML 4.12.1).
/// * An `async` script runs after the parser-blocking ones, then each `defer` script,
///   both in document order.
/// * A module script is not executed, and the count is reported in the result's notes.
/// * `testharness.js` is not run again. The adapter loaded it first, and loading it
///   again would reset the harness.
///
/// # Errors
///
/// A script the runner cannot serve is declined before anything runs. Deciding
/// here, rather than at the point of execution, is what keeps a test from running
/// part way and failing on a helper it never had.
pub(crate) fn plan_scripts(
    document: &Document,
    base: &url::Url,
    suite_root: &Path,
) -> Result<ScriptPlan, Declined> {
    let discovery = discover_scripts(document, base, ScriptDiscoveryLimits::default());
    let mut plan = ScriptPlan::default();
    let mut parser_blocking = Vec::new();
    let mut asynchronous = Vec::new();
    let mut deferred = Vec::new();
    for script in discovery.scripts {
        if script.module {
            plan.modules_not_run += 1;
            continue;
        }
        let Some(run) = resolve_script(script.source, suite_root)? else {
            continue;
        };
        match script.scheduling {
            ScriptScheduling::ParserBlocking => parser_blocking.push(run),
            ScriptScheduling::Async => asynchronous.push(run),
            ScriptScheduling::Defer => deferred.push(run),
        }
    }
    plan.scripts = parser_blocking
        .into_iter()
        .chain(asynchronous)
        .chain(deferred)
        .collect();
    Ok(plan)
}

/// Read one declared script from the checkout, or decline it.
///
/// `Ok(None)` is the harness, which is already loaded.
fn resolve_script(source: ScriptSource, suite_root: &Path) -> Result<Option<RunScript>, Declined> {
    let resolved_url = match source {
        ScriptSource::Inline { source } => return Ok(Some(RunScript { url: None, source })),
        ScriptSource::External { resolved_url, .. } => resolved_url,
    };
    let relative = suite_relative(&resolved_url)?;
    if relative == HARNESS_PATH {
        return Ok(None);
    }
    let file = suite_root.join(&relative);
    if !file.is_file() {
        return Err(fixture_declined(
            &resolved_url,
            "missing-fixture",
            format!("the script is not in the checkout at {relative}"),
        ));
    }
    if is_server_generated(&relative) {
        return Err(fixture_declined(
            &resolved_url,
            "server-generated-fixture",
            format!("the script at {relative} is a template or handler the WPT server generates"),
        ));
    }
    let text = std::fs::read_to_string(&file).map_err(|error| {
        fixture_declined(
            &resolved_url,
            "missing-fixture",
            format!("the script at {relative} could not be read as UTF-8 text: {error}"),
        )
    })?;
    Ok(Some(RunScript {
        url: Some(resolved_url.as_str().to_owned()),
        source: text,
    }))
}

/// The suite-relative path a script URL names.
///
/// A host the suite serves (the test's own host, or one of its subdomains) serves
/// the checkout by path, so `http://www1.web-platform.test/x.js` and `/x.js` name the
/// same file. A script from any other origin or scheme is not in the checkout.
fn suite_relative(url: &url::Url) -> Result<String, Declined> {
    let host = url.host_str().unwrap_or_default();
    let suite_host = host == WPT_HOST || host.ends_with(&format!(".{WPT_HOST}"));
    if !matches!(url.scheme(), "http" | "https") || !suite_host {
        return Err(fixture_declined(
            url,
            "script-from-elsewhere",
            format!("the script is served from {url}, which is not a host this runner serves"),
        ));
    }
    let relative = percent_decode(url.path()).trim_start_matches('/').to_owned();
    if relative.contains("{{") {
        return Err(fixture_declined(
            url,
            "server-generated-fixture",
            format!("the script path {relative} is a template the WPT server fills in"),
        ));
    }
    if relative.is_empty() || relative.split('/').any(|part| part == "..") {
        return Err(fixture_declined(
            url,
            "missing-fixture",
            format!("the script path {relative:?} does not name a file in the checkout"),
        ));
    }
    Ok(relative)
}

/// Whether the WPT server, rather than the checkout, produces a file's response: a
/// `.sub` file with `{{host}}` and `{{ports}}` substitution, or a `.py` handler.
fn is_server_generated(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    name.contains(".sub.") || name.ends_with(".py")
}

/// Add notes to a pass or a fail. Every other verdict is returned unchanged.
fn with_notes(verdict: Verdict, extra: Vec<String>) -> Verdict {
    if extra.is_empty() {
        return verdict;
    }
    match verdict {
        Verdict::Passed {
            assertions_evaluated,
            mut notes,
            subtests,
        } => {
            notes.extend(extra);
            Verdict::Passed {
                assertions_evaluated,
                notes,
                subtests,
            }
        }
        Verdict::Failed {
            assertion,
            engine_location,
            mut notes,
            subtests,
        } => {
            notes.extend(extra);
            Verdict::Failed {
                assertion,
                engine_location,
                notes,
                subtests,
            }
        }
        other => other,
    }
}

/// Runs the document lifecycle after a page's scripts (HTML "the end"): the
/// readiness becomes `interactive` and `DOMContentLoaded` is dispatched at the
/// document; the readiness becomes `complete`, and `readystatechange` fires
/// before the page's `load` event. A step that fails for any reason other than
/// the resource budget is ignored, as a listener's own throw is.
pub(crate) fn run_page_lifecycle(
    runtime: &mut JsRuntime,
    dom: &mut render_core::dom::Dom,
) -> Result<(), render_core::js::JsError> {
    let document = dom.document();
    let step = |result: Result<(), render_core::js::JsError>| match result {
        Err(error) if error.kind() == render_core::js::JsErrorKind::ResourceLimit => Err(error),
        _ => Ok(()),
    };
    runtime.set_document_ready_state(DocumentReadyState::Interactive);
    step(
        runtime
            .dispatch_dom_event(dom, document, "readystatechange", false, false, false, &[])
            .map(drop),
    )?;
    step(
        runtime
            .dispatch_dom_event(dom, document, "DOMContentLoaded", true, false, false, &[])
            .map(drop),
    )?;
    runtime.set_document_ready_state(DocumentReadyState::Complete);
    step(
        runtime
            .dispatch_dom_event(dom, document, "readystatechange", false, false, false, &[])
            .map(drop),
    )?;
    step(runtime.dispatch_page_load(dom))
}

/// A scheduled timer on the drain's virtual clock.
struct PendingTimer {
    /// Virtual time at which the timer is due, in milliseconds.
    due: f64,
    /// Request order, which breaks ties between timers due at the same time.
    sequence: u64,
    id: u64,
}

pub(crate) struct DrainOutcome {
    pub(crate) settled: bool,
    /// The first exception a timer callback threw while the loop ran.
    pub(crate) uncaught: Option<render_core::js::JsError>,
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
    //
    // Attribution to a subtest, without a per-assertion lookup. Each wrapped call
    // adds one to the counter and nothing else, so the runner costs an assertion no
    // more than it did before subtests were counted. A subtest's own count is the
    // increase between its first step (the test-state callback fires with STARTED
    // when a step begins, after the harness sets `tests.current_test`) and its
    // result. An assertion made by a subtest always falls inside that window, so a
    // zero is exact: nothing ran during the subtest's life that the counter saw.
    // When subtests interleave, another subtest's assertions can land in the window
    // too, which only ever over-counts; it never hides an assertion.
    var counted = 0;
    var STARTED = 1; // Test.prototype.phases.STARTED in testharness.js
    var started_at = new Map();
    add_test_state_callback(function (test) {
        if (test.phase === STARTED && !started_at.has(test)) {
            started_at.set(test, counted);
        }
    });
    // Every assertion function testharness.js exposes (`expose_assert` and the
    // `expose` helpers of the same family), so no assertion goes uncounted.
    ["assert_any", "assert_approx_equals", "assert_array_approx_equals",
     "assert_array_equals", "assert_between_exclusive", "assert_between_inclusive",
     "assert_class_string", "assert_equals", "assert_false", "assert_greater_than",
     "assert_greater_than_equal", "assert_idl_attribute", "assert_implements",
     "assert_implements_optional", "assert_in_array", "assert_inherits",
     "assert_less_than", "assert_less_than_equal", "assert_not_equals",
     "assert_not_own_property", "assert_object_equals", "assert_own_property",
     "assert_readonly", "assert_regexp_match", "assert_throws_dom",
     "assert_throws_exactly", "assert_throws_js", "assert_throws_quotaexceedederror",
     "assert_true", "assert_unreached", "promise_rejects_dom", "promise_rejects_exactly",
     "promise_rejects_js", "promise_rejects_quotaexceedederror"]
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
        // A subtest that never started has no window, so it recorded nothing.
        var start = started_at.has(test) ? started_at.get(test) : counted;
        collected.push({
            name: String(test.name),
            status: Number(test.status),
            message: test.message === undefined || test.message === null
                ? null : String(test.message),
            // `asserts` is the file-wide running total when this subtest reported.
            // `own_asserts` is the increase since this subtest's first step.
            asserts: counted,
            own_asserts: counted - start
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
    /// The file-wide running total when this subtest reported.
    asserts: u32,
    /// Assertions the counter recorded between this subtest's first step and its
    /// result. Zero means none ran while it was running. See [`RESULT_SINK`].
    own_asserts: u32,
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
            // Objects in an array are separated by a comma. Without stepping over it,
            // the second `take_object` finds a `,` and the loop ends after one result,
            // so every file's verdict was the verdict of its first subtest.
            skip_ws(bytes, &mut i);
            if bytes.get(i) == Some(&b',') {
                i += 1;
            }
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
        own_asserts: number_field(object, "own_asserts").unwrap_or(0.0).max(0.0) as u32,
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
        .map(|outcome| outcome.value.to_js_string().to_lossy());

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
            subtests: None,
            mechanism: Some("result-channel-unreadable".to_owned()),
            category: Category::HarnessLimitation,
        };
    };

    judge(harvest, path)
}

/// Judge one file from the harness's own record of its subtests. Pure, so the scoring
/// rules are tested directly, without a running harness.
fn judge(harvest: Harvest, path: &str) -> Execution {
    // The harness's own account of this file's subtests, recorded for every row that
    // came from the harness. `registered` is the harness's count of tests it ran
    // (`completion.tests`); `reported` is what reached this runner; a reported subtest
    // with `own_asserts == 0` recorded no assertion while it ran.
    let registered = harvest.completion.as_ref().map(|c| c.tests);
    let reported = u32::try_from(harvest.results.len()).unwrap_or(u32::MAX);
    let evidence = SubtestEvidence {
        registered,
        reported,
        without_assertions: u32::try_from(
            harvest.results.iter().filter(|test| test.own_asserts == 0).count(),
        )
        .unwrap_or(u32::MAX),
        // The counter is cumulative across the file, so its largest value is its total.
        assertions: harvest.results.iter().map(|test| test.asserts).max().unwrap_or(0),
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
        let unreported = registered.unwrap_or(0);
        // A timeout with no result is either a file that registered no test at
        // all, which is not a testharness file, or a file whose tests started and
        // waited for something that never happened. Those are different gaps.
        let timed_out = status.map(HarnessStatus::from_number) == Some(HarnessStatus::Timeout);
        let mechanism = match (timed_out, unreported) {
            (true, 0) => "no-testharness-test",
            (true, _) => "tests-never-finished",
            (false, _) => "no-verdict-from-harness",
        };
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
                    "{given} (the harness reported {unreported} test(s) and {} result(s) \
                     reached this runner)",
                    harvest.results.len()
                ),
                None => format!(
                    "the harness reported no result and no status for this test ({unreported} \
                     registered, 0 collected)"
                ),
            }),
            assertions: 0,
            subtests: Some(evidence),
            mechanism: Some(mechanism.to_owned()),
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
        assertions: evidence.assertions,
        subtests: Some(evidence),
        mechanism: None,
        category: Category::HarnessLimitation,
    };
    for test in harvest.results {
        let status = HarnessStatus::from_number(test.status);
        if severity(status) > severity(worst.status) {
            worst.status = status;
            worst.name = test.name;
            worst.message = test.message;
        }
    }
    // A harness-level ERROR is an uncaught exception that ended the file. The subtests that
    // passed are the ones that ran before the exception, so they cannot make the file pass.
    // A test that declares `allow_uncaught_exception` never sets this status, so a file that
    // expects the exception is judged on its own subtests and not here.
    if worst.status == HarnessStatus::Pass
        && harvest
            .completion
            .as_ref()
            .is_some_and(|c| c.status == 1.0)
    {
        worst.status = HarnessStatus::Fail;
        worst.message = Some(format!(
            "the file ended with an uncaught exception ({}), so its passing subtests ran before it \
             stopped and do not cover the file",
            harvest
                .completion
                .as_ref()
                .and_then(|c| c.message.clone())
                .unwrap_or_else(|| "no message".to_owned())
        ));
        worst.mechanism = None;
        worst.category = Category::EngineDefect;
        return worst;
    }
    // A pass needs every subtest the harness registered to have reported. A registered
    // subtest with no result is a verdict this runner cannot see, and a pass computed from
    // the others would claim more than was checked. A failure among the reported ones
    // still stands, because it is a real result whatever else went unreported. A file with
    // no completion record has no registered count to check against; its pass is recorded
    // with that gap in its evidence rather than refused here.
    let unreported = registered.unwrap_or(0).saturating_sub(reported);
    if worst.status == HarnessStatus::Pass && unreported > 0 {
        worst.status = HarnessStatus::Unknown;
        worst.message = Some(format!(
            "{unreported} of {} registered subtest(s) reported no result",
            registered.unwrap_or(0)
        ));
        worst.mechanism = Some("subtest-unreported".to_owned());
        worst.category = Category::HarnessLimitation;
        return worst;
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
        let subtests = self.subtests;
        match self.status {
            // A pass is a pass only if the run recorded an assertion.
            //
            // The measured run this replaces reported 210 passes, and **all 210
            // evaluated zero assertions**: the sink was reading `test.asserts`,
            // a field `testharness.js` does not define, so every count came back
            // `undefined` and became 0. The number looked entirely healthy - a
            // 7.4% conformance rate - and every single one of those passes was
            // vacuous.
            //
            // So a file whose subtests recorded no assertion at all is **not** a pass
            // here. It becomes an explained skip. That is the conservative direction: it
            // can only lower the reported rate, never raise it. The reason is written from
            // the run's own counter, not from the source text.
            HarnessStatus::Pass if self.assertions == 0 => Verdict::Unsupported {
                capability: None,
                reason: format!(
                    "the harness reported PASS for {} subtest(s), but this runner's assertion \
                     counter recorded no assertion across the file. A pass that checked nothing \
                     is not evidence about the engine, so it is not counted as one.",
                    self.subtests.map_or(0, |s| s.reported)
                ),
                subtests,
            },
            HarnessStatus::Pass => Verdict::Passed {
                assertions_evaluated: self.assertions,
                notes: self.notes(),
                subtests,
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
                subtests,
            },
            HarnessStatus::PreconditionFailed => Verdict::Unsupported {
                capability: None,
                reason: format!(
                    "the test declared a precondition this engine does not meet: {}",
                    self.message.as_deref().unwrap_or("<no message>")
                ),
                subtests,
            },
            HarnessStatus::NotRun => Verdict::Unsupported {
                capability: None,
                reason: format!(
                    "the test did not run to a verdict: {}",
                    self.message.as_deref().unwrap_or("<no message>")
                ),
                subtests,
            },
            // TIMEOUT is the harness's own timeout firing, not this runner's drain
            // bound (that is `event-loop-never-settles`). The file either registered
            // no test, or its tests started and waited for something that never
            // happened; the mechanism says which, see `read_results`.
            HarnessStatus::Timeout => Verdict::Unsupported {
                capability: None,
                reason: match self.mechanism.as_deref() {
                    Some("no-testharness-test") => "the file registered no testharness test \
                        before the harness timeout, so it has no testharness verdict. It is a \
                        reftest, crashtest, manual or support file."
                        .to_owned(),
                    Some("tests-never-finished") => "the file's tests started, and none \
                        finished before the harness timeout: each waited for an event, step or \
                        driver that the engine did not provide."
                        .to_owned(),
                    _ => format!(
                        "the test did not finish before the harness timeout: {}",
                        self.message.as_deref().unwrap_or("no message")
                    ),
                },
                subtests,
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
                subtests,
            },
        }
        .with_ctx(ctx)
    }

    /// Notes for a row, written from the harness's own subtest record.
    ///
    /// Each note states a fact the run recorded. None of them compares the run
    /// with a count of assertion call sites in the source.
    fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        let Some(evidence) = self.subtests else {
            return notes;
        };
        match evidence.registered {
            None => notes.push(
                "the harness gave no completion record, so the number of subtests it \
                 registered is unknown"
                    .to_owned(),
            ),
            Some(registered) if registered != evidence.reported => notes.push(format!(
                "the harness registered {registered} subtest(s) and {} reported a result to \
                 this runner",
                evidence.reported
            )),
            Some(_) => {}
        }
        if evidence.without_assertions > 0 {
            notes.push(format!(
                "{} of {} reported subtest(s) recorded no assertion while they ran, so their \
                 own counts are zero",
                evidence.without_assertions, evidence.reported
            ));
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
pub(crate) fn first_unsupported_capability(ctx: &TestContext<'_>) -> Option<Capability> {
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
            // The control drives no harness, so there is no subtest record to report.
            subtests: None,
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
    // Every area is walked first, so a reference page that a test in one area names is known
    // to be a reference when its own row is reached in another.
    let walks: Vec<(&str, Vec<String>)> = areas
        .iter()
        .map(|area| (*area, walk_area(suite_root, area)))
        .collect();
    crate::wptreftest::register_reference_targets(
        suite_root,
        walks.iter().flat_map(|(_, paths)| paths.iter().map(String::as_str)),
    );
    let mut out = Vec::new();
    for (area, paths) in walks {
        // Counted first so progress can be a fraction of a known total. A
        // "still running" message with no denominator is the same sin as a
        // percentage without one: a reader cannot tell a slow run from a stuck
        // one, and a run that looks stuck gets killed, losing the whole result.
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
        subtests: None,
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
    // A mechanism the adapter already attributed (a declined fixture) is the
    // strongest evidence there is, because the test never ran to produce a failure.
    if let Some(mechanism) = &result.mechanism {
        return Some(mechanism.clone());
    }
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
    "Notification", "Selection", "ShadowRoot", "TouchEvent",
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
    // Reference and crash test failures are named by the scorer that produced them,
    // and their text is matched whole, before the harness-prefix stripping below.
    if let Some(key) = crate::reftest::mechanism_for_assertion(assertion) {
        return key.to_owned();
    }
    // An uncaught exception in a reference or crash test is classified by the exception it
    // names, exactly as a testharness failure is, so a missing API keeps its name.
    if let Some(message) = crate::reftest::uncaught_error_message(assertion) {
        return failure_mechanism(message);
    }
    // The harness prefixes the message with the test name and a colon; strip it
    // so the pattern search sees the actual failure. Only the *first* colon is
    // consumed, because a message may legitimately contain more.
    let message = assertion
        .split_once(": ")
        .map_or(assertion, |(_, rest)| rest);

    // `testdriver-vendor.js` is the in-page half of WPT's automation protocol. Its
    // stubs throw this message because the browser automation behind them is not
    // provided to this runner, so the test cannot pass for any engine reason.
    if message.contains("is not implemented by testdriver-vendor.js") {
        return "needs-testdriver".to_owned();
    }

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
    // The census finding is a static reading of the file. When the run's own counter
    // saw assertions, the row says so, so the two readings are visible side by side.
    if previous.assertions_evaluated > 0 {
        notes.push(format!(
            "the run's assertion counter recorded {} assertion(s) for this file; the census \
             finding above is static and does not reflect them",
            previous.assertions_evaluated
        ));
    }
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
        subtests: previous.subtests,
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
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    // Helpers under resources/ and support/ are loaded by tests with
                    // <script src>. WPT does not run them as tests, so they are not rows.
                    if !crate::census::is_helper_path(&rel) {
                        out.push(rel);
                    }
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
    use super::{Execution, HarnessStatus, Harvest, judge, severity, slug};
    use crate::engine::{TestContext, Verdict};
    use crate::mechanism::Category;
    use crate::outcome::Outcome;
    use render_core::document::Document;

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
        for name in ["AbortSignal", "MutationObserver", "ResizeObserver"] {
            let key = super::failure_mechanism(&format!(
                "dom/x/y-crash.html: {name} is not defined (the harness reported 0 test(s))"
            ));
            assert_eq!(key, format!("lacks-{name}"));
        }
        // `Range` was removed from the list once the engine implemented it, so a message
        // naming it as undefined is an undefined global, not a missing interface.
        let key = super::failure_mechanism(
            "dom/x/y-crash.html: Range is not defined (the harness reported 0 test(s))",
        );
        assert_eq!(key, "undefined-global-range");
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

    #[test]
    fn a_harvest_reads_every_subtest_not_only_the_first() {
        // Every subtest after the first used to be dropped: the array's separators were
        // never stepped over, so the verdict of a file was its first subtest's verdict.
        let json = r#"{"results":[{"name":"a","status":0,"message":null,"asserts":1},{"name":"b","status":1,"message":"x","asserts":2},{"name":"c","status":0,"message":null,"asserts":3}],"completion":{"status":0,"message":null,"num_tests":3}}"#;
        let harvest = Harvest::parse(json).expect("a well-formed harvest parses");
        assert_eq!(harvest.results.len(), 3);
        assert_eq!(harvest.results[1].name, "b");
        assert_eq!(harvest.results[2].name, "c");
        assert_eq!(harvest.completion.as_ref().map(|c| c.tests), Some(3));
    }

    fn context(path: &str) -> TestContext<'_> {
        TestContext {
            path,
            area: "dom",
            file: std::path::Path::new(path),
            suite_root: std::path::Path::new("."),
            source: "",
            assertion_sites: 2,
        }
    }

    fn judged(json: &str) -> (Execution, Verdict) {
        let harvest = Harvest::parse(json).expect("the fixture is the sink's shape");
        let execution = judge(harvest, "dom/a.html");
        let verdict = execution.clone().into_verdict(&context("dom/a.html"));
        (execution, verdict)
    }

    #[test]
    fn a_harvest_carries_each_subtests_own_assertion_count() {
        let json = r#"{"results":[{"name":"a","status":0,"message":null,"asserts":2,"own_asserts":2},{"name":"b","status":0,"message":null,"asserts":2,"own_asserts":0}],"completion":{"status":0,"message":null,"num_tests":2}}"#;
        let harvest = Harvest::parse(json).expect("the sink's shape parses");
        assert_eq!(harvest.results[0].own_asserts, 2);
        assert_eq!(harvest.results[1].own_asserts, 0);
        assert_eq!(harvest.results[1].asserts, 2, "the file-wide total is kept too");
    }

    #[test]
    fn a_pass_whose_every_subtest_evaluated_an_assertion_has_full_evidence() {
        let json = r#"{"results":[{"name":"a","status":0,"message":null,"asserts":1,"own_asserts":1},{"name":"b","status":0,"message":null,"asserts":2,"own_asserts":1}],"completion":{"status":0,"message":null,"num_tests":2}}"#;
        let (execution, verdict) = judged(json);
        assert_eq!(execution.status, HarnessStatus::Pass);
        let Verdict::Passed { assertions_evaluated, notes, subtests } = verdict else {
            panic!("a fully evidenced pass stays a pass");
        };
        assert_eq!(assertions_evaluated, 2);
        assert!(notes.is_empty(), "nothing weakens this pass: {notes:?}");
        let evidence = subtests.expect("the harness record travels with the verdict");
        assert!(evidence.is_full());
        assert_eq!(evidence.registered, Some(2));
        assert_eq!(evidence.reported, 2);
    }

    #[test]
    fn a_pass_with_a_subtest_that_evaluated_nothing_stays_a_pass_but_says_so() {
        // A no-throw subtest is a legitimate WPT check, so the pass is not refused here.
        // What the run recorded is written on the row instead of being hidden.
        let json = r#"{"results":[{"name":"a","status":0,"message":null,"asserts":2,"own_asserts":2},{"name":"b","status":0,"message":null,"asserts":2,"own_asserts":0}],"completion":{"status":0,"message":null,"num_tests":2}}"#;
        let (_, verdict) = judged(json);
        let Verdict::Passed { notes, subtests, .. } = verdict else {
            panic!("the file-level pass is unchanged by a zero-count subtest");
        };
        let evidence = subtests.expect("evidence present");
        assert_eq!(evidence.without_assertions, 1);
        assert!(!evidence.is_full());
        assert!(
            notes.iter().any(|n| n.contains("1 of 2 reported subtest(s)")),
            "the zero-count subtest is named in the notes: {notes:?}"
        );
    }

    #[test]
    fn a_pass_that_registered_more_subtests_than_reported_is_refused() {
        let json = r#"{"results":[{"name":"a","status":0,"message":null,"asserts":1,"own_asserts":1},{"name":"b","status":0,"message":null,"asserts":2,"own_asserts":1}],"completion":{"status":0,"message":null,"num_tests":3}}"#;
        let (execution, verdict) = judged(json);
        assert_eq!(execution.status, HarnessStatus::Unknown);
        assert_eq!(execution.mechanism.as_deref(), Some("subtest-unreported"));
        let Verdict::Unsupported { subtests, reason, .. } = verdict else {
            panic!("an unreported subtest is an explained skip, never a pass");
        };
        assert!(reason.contains("1 of 3 registered subtest(s)"), "{reason}");
        let evidence = subtests.expect("evidence present");
        assert_eq!((evidence.registered, evidence.reported), (Some(3), 2));
    }

    #[test]
    fn a_pass_without_a_completion_record_is_kept_and_marked_as_partly_evidenced() {
        // Without a completion record the registered count is unknown. The scorer has
        // nothing to check against, so the pass stands, and the row says what is missing.
        let json = r#"{"results":[{"name":"a","status":0,"message":null,"asserts":1,"own_asserts":1}],"completion":null}"#;
        let (execution, verdict) = judged(json);
        assert_eq!(execution.status, HarnessStatus::Pass);
        let Verdict::Passed { notes, subtests, .. } = verdict else {
            panic!("the pass stands");
        };
        let evidence = subtests.expect("evidence present");
        assert_eq!(evidence.registered, None);
        assert!(!evidence.is_full());
        assert!(notes.iter().any(|n| n.contains("no completion record")), "{notes:?}");
    }

    #[test]
    fn a_file_whose_subtests_recorded_no_assertion_is_not_a_pass() {
        let json = r#"{"results":[{"name":"a","status":0,"message":null,"asserts":0,"own_asserts":0}],"completion":{"status":0,"message":null,"num_tests":1}}"#;
        let (execution, verdict) = judged(json);
        assert_eq!(execution.status, HarnessStatus::Pass);
        let Verdict::Unsupported { reason, subtests, .. } = verdict else {
            panic!("a pass with no recorded assertion is an explained skip");
        };
        assert!(reason.contains("recorded no assertion across the file"), "{reason}");
        assert_eq!(subtests.map(|s| s.reported), Some(1));
    }

    #[test]
    fn an_uncaught_exception_fails_the_file_whatever_its_subtests_recorded() {
        let json = r#"{"results":[{"name":"a","status":0,"message":null,"asserts":1,"own_asserts":1}],"completion":{"status":1,"message":"boom","num_tests":1}}"#;
        let (execution, verdict) = judged(json);
        assert_eq!(execution.status, HarnessStatus::Fail);
        assert!(matches!(verdict, Verdict::Failed { .. }));
    }

    #[test]
    fn a_subtest_failure_keeps_its_evidence_on_the_fail_row() {
        let json = r#"{"results":[{"name":"a","status":0,"message":null,"asserts":1,"own_asserts":1},{"name":"b","status":1,"message":"expected 1 but got 2","asserts":2,"own_asserts":1}],"completion":{"status":0,"message":null,"num_tests":2}}"#;
        let (execution, verdict) = judged(json);
        assert_eq!(execution.status, HarnessStatus::Fail);
        let Verdict::Failed { subtests, .. } = verdict else {
            panic!("a real failure stays a failure");
        };
        assert_eq!(subtests.map(|s| (s.registered, s.reported)), Some((Some(2), 2)));
    }

    fn base() -> url::Url {
        url::Url::parse("http://web-platform.test/a/test.html").expect("test URL")
    }

    #[test]
    fn a_script_url_on_a_suite_host_maps_onto_the_checkout() {
        // The query and fragment are not part of the file, and a subdomain of the
        // suite host serves the checkout by the same path.
        let own = url::Url::parse("http://web-platform.test/resources/x.js?q=1#f").expect("url");
        assert_eq!(super::suite_relative(&own).expect("maps"), "resources/x.js");
        let sub = url::Url::parse("http://www1.web-platform.test:8000/a%20b.js").expect("url");
        assert_eq!(super::suite_relative(&sub).expect("maps"), "a b.js");
    }

    #[test]
    fn a_script_from_another_origin_is_declined_under_its_own_mechanism() {
        let foreign = url::Url::parse("http://example.com/x.js").expect("url");
        let declined = super::suite_relative(&foreign).err().expect("declined");
        assert_eq!(declined.mechanism, "script-from-elsewhere");
    }

    #[test]
    fn server_generated_scripts_are_recognised_and_static_ones_are_not() {
        assert!(super::is_server_generated("common/get-host-info.sub.js"));
        assert!(super::is_server_generated("common/slow.py"));
        assert!(!super::is_server_generated("resources/testharnessreport.js"));
    }

    #[test]
    fn percent_escapes_decode_and_malformed_ones_are_kept() {
        assert_eq!(super::percent_decode("/a%20b%zz%"), "/a b%zz%");
        assert_eq!(super::percent_decode("/%E2%9C%93.js"), "/\u{2713}.js");
    }

    #[test]
    fn a_declared_script_missing_from_the_checkout_is_declined_before_anything_runs() {
        let document = Document::parse(
            "<script src=\"/resources/absent-helper.js\"></script><script>x()</script>",
        );
        let declined = super::plan_scripts(
            &document,
            &base(),
            std::path::Path::new("/nonexistent-suite-root"),
        )
        .err()
        .expect("declined");
        assert_eq!(declined.mechanism, "missing-fixture");
        assert!(declined.reason.contains("absent-helper.js"), "{}", declined.reason);
    }

    #[test]
    fn inline_scripts_keep_document_order_and_module_scripts_are_not_run() {
        let document = Document::parse(
            "<script>first()</script><script type=module>module()</script><script>second()</script>",
        );
        let plan = super::plan_scripts(&document, &base(), std::path::Path::new("/nonexistent"))
            .expect("plans");
        let sources: Vec<&str> = plan.scripts.iter().map(|s| s.source.as_str()).collect();
        assert_eq!(sources, ["first()", "second()"]);
        assert_eq!(plan.modules_not_run, 1);
        assert_eq!(plan.notes().len(), 1);
    }

    #[test]
    fn a_templated_script_path_is_declined_as_server_generated() {
        let templated = url::Url::parse("http://web-platform.test/a/{{host}}.js").expect("url");
        let declined = super::suite_relative(&templated).err().expect("declined");
        assert_eq!(declined.mechanism, "server-generated-fixture");
    }
}
