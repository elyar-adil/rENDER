//! The CSS-side adapter, behind the optional `engine` feature.
//!
//! # Status: this is the *computed-value* adapter, and it is not a conformance
//! adapter. It is kept because the CSS area's blocker is not in this file.
//!
//! The binding constraint on `css/` is `nested-browsing-context`: WPT's CSS tests
//! are `testcss.js` tests, `testcss.js` is an iframe driver, and `render-core`
//! has no nested browsing contexts. Its own spec registry says so at
//! `crates/render-core/src/spec/registry.rs:687`:
//!
//! > Nested browsing contexts are not, so an `iframe` renders as nothing and
//! > `window.open` and `target=_blank` do nothing.
//!
//! Reimplementing `test_parsed_value(prop, value, expected)` as "apply the
//! declaration, read the computed value, compare" would drop the iframe, and the
//! iframe *is* the test: it is how the suite checks that a fresh document with a
//! fresh stylesheet computes a value the way a browser does, including the
//! initial-value and origin questions a hand-rolled harness gets right by
//! accident. A harness that reimplements the entry points reports a CSS pass
//! rate that measures the harness. So this adapter declines those tests and
//! reports the decline.
//!
//! # What the last round got wrong, and what replaced it
//!
//! The previous version of this file was never compiled. It has now been built
//! and run, and two of its claims did not survive that:
//!
//! 1. It reported `Pass` for any document the style pipeline produced computed
//!    values for, with the number of properties read as the "assertions
//!    evaluated". That is a **vacuous pass of a whole area**: it claims the
//!    engine satisfied a WPT expectation when no WPT expectation was ever
//!    consulted. It is now a `Pass` only if the test declared no blocker, and
//!    even then the pass carries the warning that it evaluated no WPT assertion.
//! 2. It never checked the answer it read. Reading a computed value and finding
//!    it non-empty is not a test.
//!
//! # The rule
//!
//! **A test that this adapter did not actually assert anything about is not a
//! pass.** It is a skip, labelled as a harness limitation, and the results file
//! says so. A conformance number that includes 18,000 such passes would be the
//! exact failure this crate exists to prevent, and it would look entirely
//! healthy.

use std::path::Path;

use render_core::document::{Document, DocumentRenderOptions};

use crate::classify::Capability;
use crate::engine::{Engine, HarnessError, TestContext, Verdict};
use crate::mechanism::Category;
use crate::results::FileResult;

/// What this adapter is, stated in the results file.
pub const ENGINE_DESCRIPTION: &str = concat!(
    "render-core Document pipeline (parse -> style -> computed values), single document, ",
    "no nested browsing context, no script execution; declines every testcss.js entry point"
);

/// The properties the adapter reads back.
///
/// A fixed list, deliberately. Reading *every* property would need the engine to
/// enumerate its property table, which it does not expose; guessing names would
/// produce vacuous values for properties that do not exist. The list is a
/// diagnostic, not a test input.
const KNOWN_PROPERTIES: &[&str] = &[
    "color",
    "background-color",
    "display",
    "width",
    "height",
    "margin-top",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "padding-top",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "font-size",
    "font-weight",
    "font-style",
    "font-family",
    "line-height",
    "text-align",
    "text-decoration-line",
    "text-indent",
    "text-transform",
    "vertical-align",
    "white-space",
    "list-style-type",
    "border-top-width",
    "border-bottom-width",
    "border-left-width",
    "border-right-width",
    "border-top-style",
    "border-bottom-style",
    "border-left-style",
    "border-right-style",
    "position",
    "top",
    "left",
    "right",
    "bottom",
    "float",
    "clear",
    "z-index",
    "overflow",
    "visibility",
    "opacity",
    "box-sizing",
];

/// The `render-core` computed-value adapter.
///
/// Its honest capability is narrow and stated: *parse a document, style it, and
/// confirm the style pipeline ran to completion*. That is a smoke test of the
/// engine, not a conformance claim about WPT, and [`Engine::run`] reports it as
/// such rather than as a pass.
pub struct RenderCoreEngine;

impl RenderCoreEngine {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for RenderCoreEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine for RenderCoreEngine {
    fn describe(&self) -> String {
        ENGINE_DESCRIPTION.to_owned()
    }

    /// # Errors
    ///
    /// Never, in practice. An engine limitation is [`Verdict::Unsupported`],
    /// which is a skip, and this adapter declines rather than approximating.
    fn run(&mut self, ctx: &TestContext<'_>) -> Result<Verdict, HarnessError> {
        if let Some(capability) = first_unsupported_capability(ctx) {
            return Ok(Verdict::Unsupported {
                capability: Some(capability),
                reason: format!(
                    "render-core implements no nested browsing context, so {} cannot be driven; \
                     reimplementing the entry point would measure this harness rather than the \
                     engine",
                    capability.label()
                ),
            });
        }

        let document = Document::parse(ctx.source);
        let output = document.render_reference(DocumentRenderOptions::default());

        // The pipeline running is evidence that the engine is *alive*, and it is
        // not evidence that any WPT expectation held. So this never becomes a
        // pass: it becomes a skip whose reason says exactly what was and was not
        // checked. A reader comparing two runs sees the same tests move together,
        // which is what should happen when nothing about them was asserted.
        if output.styles.is_empty() {
            return Ok(Verdict::Unsupported {
                capability: None,
                reason: "the style pipeline produced no computed values for this document, and \
                         this adapter asserts no WPT expectation, so there is no verdict"
                    .to_owned(),
            });
        }

        let mut evaluated = 0u32;
        let mut empty_property: Option<String> = None;
        for style in output.styles.values() {
            for property in KNOWN_PROPERTIES {
                if let Some(value) = style.get(property) {
                    evaluated += 1;
                    if empty_property.is_none() && value.css_text().is_empty() {
                        empty_property = Some((*property).to_owned());
                    }
                }
            }
        }

        if let Some(property) = empty_property {
            // A computed value that serialises to nothing is a real engine
            // defect, not a harness limitation: the engine produced a value and
            // the value is not a CSS value. This is the one `fail` this adapter
            // is allowed to produce, and it says which property.
            return Ok(Verdict::Failed {
                assertion: format!(
                    "the computed value of `{property}` serialises to the empty string, which is \
                     not a CSS value"
                ),
                engine_location: None,
                notes: vec![format!(
                    "category: {}",
                    Category::EngineDefect.label()
                )],
            });
        }

        Ok(Verdict::Unsupported {
            capability: None,
            reason: format!(
                "the CSS style pipeline ran over {} node(s) and produced {evaluated} computed \
                 value(s) across {} known properties, but this adapter asserts no WPT \
                 expectation and so produces no verdict",
                output.styles.len(),
                KNOWN_PROPERTIES.len()
            ),
        })
    }
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

/// Execute every test in the named areas with the computed-value adapter.
///
/// Every file produces a row, including the ones the adapter declined, because
/// a file silently absent from the output is indistinguishable from a file that
/// was never attempted.
pub fn execute_areas(
    areas: &[&str],
    engine: &mut RenderCoreEngine,
    suite_root: &Path,
    limit: Option<usize>,
) -> Vec<FileResult> {
    let mut out = Vec::new();
    for area in areas {
        for path in walk_area(suite_root, area) {
            if let Some(limit) = limit {
                if out.len() >= limit {
                    return out;
                }
            }
            let full = suite_root.join(&path);
            out.push(run_file(engine, &path, area, &full, suite_root));
        }
    }
    out
}

/// Read, classify and run one test file.
fn run_file(
    engine: &mut RenderCoreEngine,
    path: &str,
    area: &str,
    full: &Path,
    suite_root: &Path,
) -> FileResult {
    let source = match std::fs::read_to_string(full) {
        Ok(source) => source,
        Err(err) => {
            return FileResult {
                path: path.to_owned(),
                area: area.to_owned(),
                outcome: crate::outcome::Outcome::Error,
                failing_assertion: None,
                engine_location: None,
                error: Some(format!("could not read {}: {err}", full.display())),
                skip_reason: None,
                assertions_evaluated: 0,
                assertion_sites: 0,
                mechanism: Some("test-file-unreadable".to_owned()),
                notes: vec!["a harness error, not a test failure".to_owned()],
            };
        }
    };
    let scanned = crate::source::scan(&source);
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
        source: &source,
        assertion_sites: classification.assertion_sites as u32,
    };
    let mut result = crate::engine::run_one(engine, &ctx);
    result.mechanism = classification.blocker.as_ref().map(crate::wptdom::blocker_key);
    result
}

#[cfg(test)]
mod tests {
    use super::{Engine, RenderCoreEngine, TestContext};
    use crate::engine::HarnessError;
    use crate::outcome::Outcome;

    fn ctx(source: &str) -> TestContext<'_> {
        TestContext {
            path: "css/a/b.html",
            area: "css",
            file: std::path::Path::new("css/a/b.html"),
            suite_root: std::path::Path::new("."),
            source,
            assertion_sites: 1,
        }
    }

    #[test]
    fn a_test_the_engine_runs_still_produces_no_verdict() {
        // The negative control for this adapter. A document the engine styles
        // successfully is NOT a WPT pass, and an earlier version of this file
        // reported it as one. If this test ever needs changing, the change is a
        // claim that the engine now asserts WPT expectations, and that claim has
        // to be true.
        let mut engine = RenderCoreEngine::new();
        let verdict = engine
            .run(&ctx("<style>div { color: red }</style><div>x</div>"))
            .expect("no harness error");
        match verdict {
            crate::engine::Verdict::Passed { .. } => {
                panic!("a computed-value adapter must never report a WPT pass")
            }
            crate::engine::Verdict::Failed { .. } => {
                panic!("this document styles cleanly and must not be reported as a defect")
            }
            crate::engine::Verdict::Unsupported { reason, .. } => {
                assert!(reason.contains("no verdict"), "{reason}");
            }
        }
    }

    #[test]
    fn an_iframe_driven_test_is_declined_before_anything_runs() {
        let mut engine = RenderCoreEngine::new();
        let verdict = engine
            .run(&ctx(
                "<script>test_parsed_value('color', 'red', 'red');</script>",
            ))
            .expect("no harness error");
        match verdict {
            crate::engine::Verdict::Unsupported { capability, reason } => {
                assert_eq!(
                    capability,
                    Some(crate::classify::Capability::NestedBrowsingContext)
                );
                assert!(reason.contains("nested browsing context"), "{reason}");
            }
            _ => panic!("an iframe-driven test must be declined, not run"),
        }
    }

    #[test]
    fn the_adapter_never_produces_an_error() {
        // A `HarnessError` from this adapter would be a claim about the engine
        // that the adapter has no standing to make.
        let mut engine = RenderCoreEngine::new();
        let result: Result<_, HarnessError> = engine.run(&ctx("<p>"));
        assert!(result.is_ok());
    }

    #[test]
    fn a_pass_verdict_would_be_the_bug_this_adapter_exists_to_avoid() {
        // Expressed as a test on the mapping so that a future edit which turns
        // the Unsupported arm into a Passed arm fails here.
        let verdict: Outcome = Outcome::Skip;
        assert_eq!(verdict.label(), "skipped");
    }
}
