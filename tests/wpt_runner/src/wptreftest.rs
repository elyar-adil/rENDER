//! Score reference tests and crash tests through render-core, behind the `engine`
//! feature.
//!
//! The rules are in [`crate::reftest`]. This module does the two things those
//! rules cannot: it renders a page and it says what the render did not produce.
//!
//! # What "rendered" means here
//!
//! Each page is parsed, its classic scripts run in the same order and with the
//! same lifecycle as a testharness test, its stylesheets are injected, and then it
//! is painted by render-core's deterministic reference path. That path is the one
//! the rest of the suite of WPT conformance numbers rests on, so it is the one a
//! pixel comparison uses here too.
//!
//! The reference path paints no glyphs, loads no images, runs no nested document
//! and draws no canvas bitmap. A page that depends on any of those would compare
//! equal to a page that differs in exactly that feature, and an equal raster is a
//! pass. So a page whose pixels depend on one is declined as a missing capability,
//! never compared. Declining is checked on the page's final DOM, after its scripts
//! have run, so a canvas or an iframe that a script creates is caught as well as
//! one written in the markup.

use std::collections::BTreeSet;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use render_core::document::{
    AuthorStyleSource, Document, DocumentLimits, DocumentRenderOptions, ExternalStyleSheetKey,
    ExternalStyleSheets,
};
use render_core::dom::{Dom, Node, NodeId, NodeKind};
use render_core::js::{DocumentLoader as _, JsRuntime, RuntimeLimits};
use render_core::layout::{LayoutOptions, PhysicalSize};
use render_core::paint::DisplayCommand;
use url::Url;

use crate::engine::{HarnessError, TestContext, Verdict};
use crate::outcome::SkipReason;
use crate::reftest::{self, Metadata, RefKind, Reference, Scope};
use crate::wptdom::{self, SuiteLoader};

/// Score a file as a reference test or a crash test, when it is one.
///
/// Returns `None` for a file that is neither, so the testharness path decides it.
/// A file that is one of them always gets a verdict here, including a skip.
///
/// # Errors
///
/// Only for harness-level failure, such as a reference file that cannot be read.
/// An engine limitation is a declined verdict.
pub fn score(ctx: &TestContext<'_>, suite_root: &Path) -> Option<Result<Verdict, HarnessError>> {
    let document = Document::parse(ctx.source);
    let meta = metadata(document.dom());
    let scope = reftest::scope(ctx.path, &meta);
    match scope {
        Scope::NotScored if !meta.has_testharness && is_reference_target(ctx.path) => {
            Some(Ok(declined(
                SkipReason::ReferenceFile,
                "a reference page: another test compares against it, and it declares no test of its own",
            )))
        }
        Scope::NotScored => None,
        Scope::Manual => Some(Ok(declined(
            SkipReason::NotAutomatable,
            "a manual test: a person has to run it, and this runner does not drive it",
        ))),
        Scope::NonScreen => Some(Ok(declined(
            SkipReason::NotAutomatable,
            "the test needs print or paged media, which this runner does not render",
        ))),
        Scope::Reftest | Scope::Crashtest => {
            if let Some(capability) = wptdom::first_unsupported_capability(ctx) {
                return Some(Ok(declined(
                    SkipReason::MissingEngineCapability,
                    &format!(
                        "the engine lacks {}, so the test cannot be driven",
                        capability.label()
                    ),
                )));
            }
            Some(match scope {
                Scope::Reftest => score_reftest(ctx, suite_root, &meta),
                _ => Ok(score_crashtest(ctx, suite_root)),
            })
        }
    }
}

/// Every suite path that a walked test names with `link rel=match|mismatch`, collected
/// once per run from all the areas being walked. A file in this set is a reference page.
static REFERENCE_TARGETS: std::sync::Mutex<BTreeSet<String>> =
    std::sync::Mutex::new(BTreeSet::new());

/// Record the reference targets of every walked file. Called before any file runs, so a
/// reference named by a test in one area is recognised when its own row is reached.
pub fn register_reference_targets<'a>(suite_root: &Path, paths: impl IntoIterator<Item = &'a str>) {
    let mut targets = BTreeSet::new();
    for path in paths {
        let Ok(source) = fs::read_to_string(suite_root.join(path)) else {
            continue;
        };
        let document = Document::parse(&source);
        for reference in metadata(document.dom()).references {
            if let Ok(target) = reftest::resolve_reference(path, &reference.href) {
                targets.insert(target);
            }
        }
    }
    if let Ok(mut registered) = REFERENCE_TARGETS.lock() {
        *registered = targets;
    }
}

fn is_reference_target(path: &str) -> bool {
    REFERENCE_TARGETS
        .lock()
        .is_ok_and(|targets| targets.contains(path))
}

/// What a file declares, read from its parsed DOM.
fn metadata(dom: &Dom) -> Metadata {
    let mut meta = Metadata::default();
    let mut pending = vec![dom.document()];
    while let Some(node) = pending.pop() {
        if let Some(NodeKind::Element(element)) = dom.node(node).map(Node::kind) {
            match element.local_name.as_str() {
                "link" => {
                    let rels = attribute(dom, node, "rel").unwrap_or_default();
                    for token in rels.split_ascii_whitespace() {
                        let kind = match token.to_ascii_lowercase().as_str() {
                            "match" => RefKind::Match,
                            "mismatch" => RefKind::Mismatch,
                            _ => continue,
                        };
                        meta.references.push(Reference {
                            kind,
                            href: attribute(dom, node, "href").unwrap_or_default(),
                        });
                    }
                }
                "meta" => {
                    let name = attribute(dom, node, "name").unwrap_or_default();
                    let content = attribute(dom, node, "content").unwrap_or_default();
                    if name.eq_ignore_ascii_case("fuzzy") {
                        meta.fuzzy.push(content);
                    } else if name.eq_ignore_ascii_case("flags") {
                        meta.flags.extend(
                            content
                                .split_ascii_whitespace()
                                .map(str::to_ascii_lowercase),
                        );
                    }
                }
                "script" => {
                    if attribute(dom, node, "src").is_some_and(|src| src.contains("testharness.js"))
                    {
                        meta.has_testharness = true;
                    }
                }
                _ => {}
            }
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().copied());
    }
    meta
}

fn attribute(dom: &Dom, node: NodeId, name: &str) -> Option<String> {
    dom.attribute(node, name).ok().flatten().map(str::to_owned)
}

/// A reference test: render the test once, render each reference, and judge.
fn score_reftest(
    ctx: &TestContext<'_>,
    suite_root: &Path,
    meta: &Metadata,
) -> Result<Verdict, HarnessError> {
    let rules = match reftest::fuzzy_rules(&meta.fuzzy) {
        Ok(rules) => rules,
        Err(reason) => return Ok(declined(SkipReason::UnsupportedHarnessShape, &reason)),
    };
    let mut targets = Vec::with_capacity(meta.references.len());
    for reference in &meta.references {
        match reftest::resolve_reference(ctx.path, &reference.href) {
            Ok(relative) => targets.push((reference, relative)),
            Err(reason) => return Ok(declined(SkipReason::UnsupportedHarnessShape, &reason)),
        }
    }

    // Every reference is read and checked for chains before anything is rendered, so a test
    // that cannot be scored is refused on what it declares, not on what its page paints.
    let mut loaded = Vec::with_capacity(targets.len());
    for (reference, relative) in targets {
        let path = suite_root.join(&relative);
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                return Ok(Verdict::MissingFixture {
                    url: relative,
                    mechanism: "missing-reference-file".to_owned(),
                    reason: format!("the reference file cannot be read: {error}"),
                });
            }
        };
        let reference_meta = metadata(Document::parse(&source).dom());
        if !reference_meta.references.is_empty() {
            return Ok(declined(
                SkipReason::UnsupportedHarnessShape,
                &format!(
                    "the reference {relative} is itself a reftest, and reference chains are not scored"
                ),
            ));
        }
        loaded.push((reference, relative, source));
    }

    let test = match render_document(suite_root, ctx.path, ctx.source, Checks::Reference) {
        Ok(page) => page,
        Err(refusal) => return Ok(refusal.into_verdict()),
    };

    let mut comparisons = Vec::with_capacity(loaded.len());
    for (reference, relative, source) in loaded {
        let page = match render_document(suite_root, &relative, &source, Checks::Reference) {
            Ok(page) => page,
            Err(refusal) => return Ok(refusal.into_verdict()),
        };
        let fuzziness = reftest::fuzziness_for(&rules, ctx.path, &reference.href);
        let difference = (test.width == page.width && test.height == page.height)
            .then(|| reftest::difference(&test.pixels, &page.pixels));
        let equal = reftest::within(difference, fuzziness);
        comparisons.push(Comparison {
            kind: reference.kind,
            relative,
            equal,
            difference,
            fuzziness,
        });
    }

    let outcomes: Vec<(RefKind, bool)> = comparisons.iter().map(|c| (c.kind, c.equal)).collect();
    Ok(match reftest::judge(&outcomes) {
        Ok(()) => Verdict::Passed {
            assertions_evaluated: u32::try_from(comparisons.len()).unwrap_or(u32::MAX),
            notes: comparisons.iter().map(Comparison::describe).collect(),
            subtests: None,
        },
        Err(reftest::Failure::MatchDiffers) => {
            let detail = comparisons
                .iter()
                .map(Comparison::describe)
                .collect::<Vec<_>>()
                .join("; ");
            Verdict::Failed {
                assertion: format!("{}: {detail}", reftest::DIFFER_PREFIX),
                engine_location: None,
                notes: Vec::new(),
                subtests: None,
            }
        }
        Err(reftest::Failure::MismatchMatches { index }) => {
            let detail = comparisons
                .get(index)
                .map(Comparison::describe)
                .unwrap_or_default();
            Verdict::Failed {
                assertion: format!("{}: {detail}", reftest::MATCH_PREFIX),
                engine_location: None,
                notes: Vec::new(),
                subtests: None,
            }
        }
    })
}

/// One reference compared with the test.
struct Comparison {
    kind: RefKind,
    relative: String,
    equal: bool,
    difference: Option<reftest::Difference>,
    fuzziness: Option<reftest::Fuzziness>,
}

impl Comparison {
    /// The comparison in words, with the measured difference and the tolerance.
    fn describe(&self) -> String {
        let relation = match self.kind {
            RefKind::Match => "matches",
            RefKind::Mismatch => "is compared with",
        };
        let measured = match self.difference {
            Some(d) => format!(
                "max channel difference {}, {} pixel(s) differ",
                d.max_per_channel, d.pixels_different
            ),
            None => "the surfaces differ in size".to_owned(),
        };
        let allowed = self.fuzziness.map_or_else(
            || "exact".to_owned(),
            |f| {
                format!(
                    "allowed max difference {}-{}, pixels {}-{}",
                    f.max_difference.0, f.max_difference.1, f.total_pixels.0, f.total_pixels.1
                )
            },
        );
        format!(
            "test {relation} {}: {measured} ({allowed}); {}",
            self.relative,
            if self.equal { "equal" } else { "not equal" }
        )
    }
}

/// A crash test: the page must load, run and paint without a panic or an
/// uncaught error. A panic is the finding, so it is a fail rather than an error.
fn score_crashtest(ctx: &TestContext<'_>, suite_root: &Path) -> Verdict {
    let attempt = catch_unwind(AssertUnwindSafe(|| {
        render_document(suite_root, ctx.path, ctx.source, Checks::Crash)
    }));
    match attempt {
        Err(payload) => Verdict::Failed {
            assertion: format!(
                "{}: {}",
                reftest::CRASHTEST_PANIC_PREFIX,
                panic_text(payload.as_ref())
            ),
            engine_location: None,
            notes: vec!["the engine panicked while the crash test was loaded".to_owned()],
            subtests: None,
        },
        Ok(Err(refusal)) => refusal.into_verdict(),
        Ok(Ok(_)) => Verdict::Passed {
            assertions_evaluated: 1,
            notes: vec![
                "the page loaded, ran its scripts and painted without a panic or an uncaught error"
                    .to_owned(),
            ],
            subtests: None,
        },
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn declined(skip_reason: SkipReason, reason: &str) -> Verdict {
    Verdict::Declined {
        skip_reason,
        reason: reason.to_owned(),
    }
}

/// Which checks a page must pass before its pixels are compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Checks {
    /// A page whose pixels are compared: every feature check applies.
    Reference,
    /// A crash test: only the lifecycle, a panic and an uncaught error matter.
    Crash,
}

/// A rendered page, as the rows of RGB pixels a comparison reads.
struct Rendered {
    width: u32,
    height: u32,
    pixels: Vec<[u8; 3]>,
}

/// Why a page was not rendered.
#[derive(Debug)]
enum Refusal {
    /// The page's pixels cannot be produced by the reference render.
    Declined {
        skip_reason: SkipReason,
        reason: String,
    },
    /// A file the page declares is not in the checkout.
    Missing {
        url: String,
        mechanism: String,
        reason: String,
    },
    /// The page failed: an uncaught error, with the assertion text that names it.
    Failed(String),
}

impl Refusal {
    fn into_verdict(self) -> Verdict {
        match self {
            Self::Declined {
                skip_reason,
                reason,
            } => Verdict::Declined {
                skip_reason,
                reason,
            },
            Self::Missing {
                url,
                mechanism,
                reason,
            } => Verdict::MissingFixture {
                url,
                mechanism,
                reason,
            },
            Self::Failed(assertion) => Verdict::Failed {
                assertion,
                engine_location: None,
                notes: Vec::new(),
                subtests: None,
            },
        }
    }
}

const REFTEST_WAIT: &str = "reftest-wait";

/// Parse, run the page's scripts through the document lifecycle, check it, and
/// paint it through the reference path.
fn render_document(
    suite_root: &Path,
    relative: &str,
    source: &str,
    checks: Checks,
) -> Result<Rendered, Refusal> {
    let base = wptdom::test_url(relative);
    let mut document = Document::parse(source);
    let plan = wptdom::plan_scripts(&document, &base, suite_root).map_err(|declined| {
        Refusal::Missing {
            url: declined.url,
            mechanism: declined.mechanism.to_owned(),
            reason: declined.reason,
        }
    })?;

    // A module script is not executed by this runner, so a page that declares one would be
    // painted without the code that paints it. That is declined, never rendered short.
    if plan.modules_not_run > 0 {
        return Err(Refusal::Declined {
            skip_reason: SkipReason::UnverifiedCapability,
            reason: "the page declares a module script, which this runner does not execute"
                .to_owned(),
        });
    }

    let limits = RuntimeLimits::default();
    let mut runtime = JsRuntime::with_limits_and_url(document.dom(), limits, &base);
    runtime.set_document_loader(Box::new(SuiteLoader {
        root: suite_root.to_path_buf(),
    }));
    runtime.install_canvas_host(Box::new(render_core::canvas::CanvasHandle::default()));

    // The first uncaught error is kept, and the rest of the page still runs, as HTML's
    // "report the exception" requires. Whether it fails the page is decided below,
    // after the feature checks, so a page that is declined is declined whatever else
    // it did.
    let mut uncaught: Option<String> = None;
    for script in &plan.scripts {
        let outcome = match &script.url {
            Some(url) => runtime.execute_with_filename(document.dom_mut(), &script.source, url),
            None => runtime.execute(document.dom_mut(), &script.source),
        };
        if let Err(error) = outcome {
            if error.kind() == render_core::js::JsErrorKind::ResourceLimit {
                return Err(budget_exhausted(&error.to_string()));
            }
            uncaught.get_or_insert_with(|| error.to_string());
        }
    }
    if let Err(error) = wptdom::run_page_lifecycle(&mut runtime, document.dom_mut())
        && error.kind() == render_core::js::JsErrorKind::ResourceLimit
    {
        return Err(budget_exhausted(&error.to_string()));
    }
    let drained = wptdom::WptHarnessEngine::drain(&mut runtime, document.dom_mut());
    if let Some(error) = drained.uncaught {
        if error.kind() == render_core::js::JsErrorKind::ResourceLimit {
            return Err(budget_exhausted(&error.to_string()));
        }
        uncaught.get_or_insert_with(|| error.to_string());
    }
    if !drained.settled {
        return Err(Refusal::Declined {
            skip_reason: SkipReason::UnverifiedCapability,
            reason: "the page's event loop did not settle, so its final state is not known"
                .to_owned(),
        });
    }
    // A page that holds its screenshot with `reftest-wait` is released by its own
    // `TestRendered` handler, as WPT's reftest harness dispatches it.
    if root_has_class(document.dom(), REFTEST_WAIT) {
        let root = root_element(document.dom());
        if let Some(root) = root
            && let Err(error) = runtime.dispatch_dom_event(
                document.dom_mut(),
                root,
                "TestRendered",
                true,
                false,
                false,
                &[],
            )
        {
            if error.kind() == render_core::js::JsErrorKind::ResourceLimit {
                return Err(budget_exhausted(&error.to_string()));
            }
            uncaught.get_or_insert_with(|| error.to_string());
        }
        let drained = wptdom::WptHarnessEngine::drain(&mut runtime, document.dom_mut());
        if let Some(error) = drained.uncaught {
            if error.kind() == render_core::js::JsErrorKind::ResourceLimit {
                return Err(budget_exhausted(&error.to_string()));
            }
            uncaught.get_or_insert_with(|| error.to_string());
        }
        if !drained.settled || root_has_class(document.dom(), REFTEST_WAIT) {
            return Err(Refusal::Declined {
                skip_reason: SkipReason::UnverifiedCapability,
                reason: "the page kept reftest-wait set after TestRendered, so it was not captured"
                    .to_owned(),
            });
        }
    }
    drop(runtime);

    if checks == Checks::Reference
        && let Some((skip_reason, reason)) = unsupported_in_reference_render(document.dom())
    {
        return Err(Refusal::Declined {
            skip_reason,
            reason: reason.to_owned(),
        });
    }
    if let Some(error) = uncaught {
        let prefix = match checks {
            Checks::Reference => reftest::REFTEST_UNCAUGHT_PREFIX,
            Checks::Crash => reftest::CRASHTEST_UNCAUGHT_PREFIX,
        };
        return Err(Refusal::Failed(format!("{prefix}: {error}")));
    }

    let external = external_style_sheets(&document, &base, suite_root, checks)?;

    let options = DocumentRenderOptions {
        layout: LayoutOptions {
            viewport: PhysicalSize {
                width: reftest::VIEWPORT_WIDTH as f32,
                height: reftest::VIEWPORT_HEIGHT as f32,
            },
            ..LayoutOptions::default()
        },
        ..DocumentRenderOptions::default()
    };
    let output = document.render_reference_with_external_style_sheets(options, &base, &external);
    if checks == Checks::Crash {
        return Ok(to_rendered(&output.raster.surface));
    }
    if let Some(stage) = diagnostic_stage(&output.diagnostics) {
        return Err(Refusal::Declined {
            skip_reason: SkipReason::MissingEngineCapability,
            reason: format!(
                "the reference render reported a {stage} diagnostic for this page, a feature it does not implement"
            ),
        });
    }
    let glyphs_painted = output.display.list.items().iter().any(|item| {
        matches!(
            &item.command,
            DisplayCommand::GlyphRun(run) if !run.glyphs.is_empty()
        )
    });
    if glyphs_painted {
        return Err(Refusal::Declined {
            skip_reason: SkipReason::UnverifiedCapability,
            reason: "the reference render does not implement glyph masks, so the page's text cannot be compared"
                .to_owned(),
        });
    }
    let translucent = output
        .raster
        .surface
        .pixels()
        .iter()
        .any(|pixel| pixel.alpha != u8::MAX);
    if translucent {
        return Err(Refusal::Declined {
            skip_reason: SkipReason::UnverifiedCapability,
            reason: "the reference render produced translucent pixels, which are not the opaque screenshot WPT compares"
                .to_owned(),
        });
    }
    let rendered = to_rendered(&output.raster.surface);
    // A page painted in one colour throughout shows nothing that can be compared. An equal
    // pair of such pages is a pass that measured nothing, and a page whose content the engine
    // did not paint looks the same. So it is declined, whichever side of the comparison it is on.
    if rendered.pixels.windows(2).all(|pair| pair[0] == pair[1]) {
        return Err(Refusal::Declined {
            skip_reason: SkipReason::UnverifiedCapability,
            reason: "the reference render painted a single colour, so the page shows nothing that can be compared"
                .to_owned(),
        });
    }
    Ok(rendered)
}

/// The first pipeline stage that reported a diagnostic, by name. A diagnostic means
/// render-core met something it does not handle, so the page is not compared.
fn diagnostic_stage(
    diagnostics: &render_core::document::DocumentRenderDiagnostics,
) -> Option<&'static str> {
    [
        ("document", !diagnostics.document.is_empty()),
        ("style-sheet", !diagnostics.style_sheets.is_empty()),
        ("computed-style", !diagnostics.computed_styles.is_empty()),
        ("formatting", !diagnostics.formatting.is_empty()),
        ("layout", !diagnostics.layout.is_empty()),
        ("display-list", !diagnostics.display_list.is_empty()),
        ("raster", !diagnostics.raster.is_empty()),
        ("inline-svg", !diagnostics.inline_svg.is_empty()),
    ]
    .into_iter()
    .find(|(_, present)| *present)
    .map(|(stage, _)| stage)
}

fn to_rendered(surface: &render_core::paint::Surface) -> Rendered {
    Rendered {
        width: surface.width(),
        height: surface.height(),
        pixels: surface
            .pixels()
            .iter()
            .map(|pixel| [pixel.red, pixel.green, pixel.blue])
            .collect(),
    }
}

fn budget_exhausted(message: &str) -> Refusal {
    Refusal::Declined {
        skip_reason: SkipReason::UnverifiedCapability,
        reason: format!(
            "the engine step budget ran out while the page's scripts ran, so its final state is not known: {message}"
        ),
    }
}

/// Load the page's external stylesheets from the checkout, after its scripts have
/// run, so a stylesheet a script adds is injected as well.
fn external_style_sheets(
    document: &Document,
    base: &Url,
    suite_root: &Path,
    checks: Checks,
) -> Result<ExternalStyleSheets, Refusal> {
    let mut loader = SuiteLoader {
        root: suite_root.to_path_buf(),
    };
    let discovery = document.discover_author_style_slots(base, DocumentLimits::default());
    let mut external = ExternalStyleSheets::default();
    for slot in discovery.slots {
        let AuthorStyleSource::External { resolved_url, .. } = slot.source else {
            continue;
        };
        let Some(url) = resolved_url else {
            return Err(Refusal::Declined {
                skip_reason: SkipReason::UnverifiedCapability,
                reason: "a stylesheet URL cannot be resolved against the test".to_owned(),
            });
        };
        if url.host_str() != Some(wptdom::WPT_HOST) {
            return Err(Refusal::Declined {
                skip_reason: SkipReason::MissingEngineCapability,
                reason: format!(
                    "a stylesheet from outside the suite ({url}) is not loaded by the reference render"
                ),
            });
        }
        let Some(sheet) = loader.load_text(&url) else {
            return Err(Refusal::Missing {
                url: url.to_string(),
                mechanism: "missing-stylesheet".to_owned(),
                reason:
                    "the stylesheet is absent from the checkout, or is generated by the WPT server"
                        .to_owned(),
            });
        };
        if checks == Checks::Reference && has_external_resource(&sheet) {
            return Err(Refusal::Declined {
                skip_reason: SkipReason::MissingEngineCapability,
                reason: "the reference render does not implement url() or @import resources in a stylesheet"
                    .to_owned(),
            });
        }
        external.insert_css(ExternalStyleSheetKey::new(slot.owner, url), &sheet);
    }
    Ok(external)
}

fn has_external_resource(css: &str) -> bool {
    let lower = css.to_ascii_lowercase();
    lower.contains("url(") || lower.contains("@import")
}

/// The first feature in the page's final DOM that the reference render does not
/// paint, and the reason, or `None` when every element is one it does paint.
fn unsupported_in_reference_render(dom: &Dom) -> Option<(SkipReason, &'static str)> {
    let mut pending = vec![dom.document()];
    while let Some(node) = pending.pop() {
        if let Some(NodeKind::Element(element)) = dom.node(node).map(Node::kind) {
            let found = match element.local_name.as_str() {
                "iframe" | "frame" | "frameset" => Some((
                    SkipReason::MissingEngineCapability,
                    "the reference render does not paint a nested document, so an iframe is not compared",
                )),
                "canvas" => Some((
                    SkipReason::MissingEngineCapability,
                    "the reference render does not paint canvas bitmaps",
                )),
                "img" | "picture" | "svg" | "video" | "audio" | "object" | "embed" => Some((
                    SkipReason::MissingEngineCapability,
                    "the reference render does not load embedded images, svg or media",
                )),
                "input"
                    if attribute(dom, node, "type")
                        .is_some_and(|t| t.eq_ignore_ascii_case("image")) =>
                {
                    Some((
                        SkipReason::MissingEngineCapability,
                        "the reference render does not load image inputs",
                    ))
                }
                // Form controls are laid out and styled, but their widget appearance is not
                // painted (the engine's registry records the appearance as deferred). A page
                // that compares them compares boxes, not the controls the test is about.
                "input" | "select" | "textarea" | "button" | "meter" | "progress" | "keygen" => {
                    Some((
                        SkipReason::MissingEngineCapability,
                        "the reference render does not paint form control appearance",
                    ))
                }
                "style" => {
                    let text: String = dom
                        .children(node)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|child| match dom.node(*child).map(Node::kind) {
                            Some(NodeKind::Text(text)) => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    has_external_resource(&text).then_some((
                        SkipReason::MissingEngineCapability,
                        "the reference render does not implement url() or @import resources in a style element",
                    ))
                }
                _ => None,
            };
            if found.is_some() {
                return found;
            }
            if attribute(dom, node, "style").is_some_and(|style| has_external_resource(&style)) {
                return Some((
                    SkipReason::MissingEngineCapability,
                    "the reference render does not implement url() resources in a style attribute",
                ));
            }
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().copied());
    }
    None
}

fn root_element(dom: &Dom) -> Option<NodeId> {
    dom.children(dom.document())?.iter().copied().find(|node| {
        matches!(
            dom.node(*node).map(Node::kind),
            Some(NodeKind::Element(element)) if element.local_name == "html"
        )
    })
}

fn root_has_class(dom: &Dom, class: &str) -> bool {
    root_element(dom)
        .and_then(|root| attribute(dom, root, "class"))
        .is_some_and(|classes| classes.split_ascii_whitespace().any(|c| c == class))
}

#[cfg(test)]
mod tests {
    use super::score;
    use crate::engine::{TestContext, Verdict};
    use crate::outcome::SkipReason;
    use crate::reftest::{
        CRASHTEST_PANIC_PREFIX, CRASHTEST_UNCAUGHT_PREFIX, DIFFER_PREFIX, MATCH_PREFIX,
        REFTEST_UNCAUGHT_PREFIX,
    };
    use std::path::{Path, PathBuf};

    /// A fixture suite under this crate's `target/`, one directory per test, so
    /// parallel tests never share a file.
    fn suite(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/wptreftest-fixtures")
            .join(name);
        let _ = std::fs::remove_dir_all(&root);
        for (relative, body) in files {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().expect("fixture has a directory"))
                .expect("fixture directory");
            // Standards mode. A page without a doctype is quirks mode, which render-core
            // reports as an unimplemented feature, so every fixture declares one.
            std::fs::write(path, format!("<!DOCTYPE html>{body}")).expect("fixture file");
        }
        root
    }

    /// Score `test` under `root` on a thread with the harness's stack size, since
    /// the engine's recursion budget assumes one.
    fn verdict(root: &Path, test: &str) -> Verdict {
        let root = root.to_path_buf();
        let test = test.to_owned();
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let source = std::fs::read_to_string(root.join(&test)).expect("test file");
                let ctx = TestContext {
                    path: &test,
                    area: "html",
                    file: &root.join(&test),
                    suite_root: &root,
                    source: &source,
                    assertion_sites: 0,
                };
                score(&ctx, &root)
                    .expect("a reftest or crash test is scored here")
                    .expect("no harness error")
            })
            .expect("test thread")
            .join()
            .expect("the scorer does not panic")
    }

    const GREEN_BOX: &str = r#"<div style="width:20px;height:20px;background:#0f0"></div>"#;

    fn assert_failed_with(verdict: &Verdict, prefix: &str) {
        match verdict {
            Verdict::Failed { assertion, .. } => assert!(
                assertion.starts_with(prefix),
                "expected a failure starting {prefix:?}, got {assertion:?}"
            ),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    fn assert_declined(verdict: &Verdict, reason: SkipReason) -> String {
        match verdict {
            Verdict::Declined {
                skip_reason,
                reason: text,
            } => {
                assert_eq!(*skip_reason, reason, "{text}");
                text.clone()
            }
            other => panic!("expected Declined, got {other:?}"),
        }
    }

    #[test]
    fn identical_pages_match_and_a_colour_difference_fails_a_match() {
        let root = suite(
            "identical",
            &[
                (
                    "test.html",
                    &format!(r#"<link rel=match href="ref.html">{GREEN_BOX}"#),
                ),
                ("ref.html", GREEN_BOX),
                (
                    "other-ref.html",
                    r#"<div style="width:20px;height:20px;background:#f00"></div>"#,
                ),
                (
                    "red.html",
                    &format!(r#"<link rel=match href="other-ref.html">{GREEN_BOX}"#),
                ),
            ],
        );
        assert!(matches!(
            verdict(&root, "test.html"),
            Verdict::Passed { .. }
        ));
        assert_failed_with(&verdict(&root, "red.html"), DIFFER_PREFIX);
    }

    #[test]
    fn a_colour_difference_passes_a_mismatch() {
        let root = suite(
            "mismatch",
            &[
                (
                    "test.html",
                    &format!(r#"<link rel=mismatch href="ref.html">{GREEN_BOX}"#),
                ),
                (
                    "ref.html",
                    r#"<div style="width:20px;height:20px;background:#f00"></div>"#,
                ),
            ],
        );
        assert!(matches!(
            verdict(&root, "test.html"),
            Verdict::Passed { .. }
        ));
        // A mismatch reference equal to the test is a failure.
        let equal = suite(
            "mismatch-equal",
            &[
                (
                    "test.html",
                    &format!(r#"<link rel=mismatch href="ref.html">{GREEN_BOX}"#),
                ),
                ("ref.html", GREEN_BOX),
            ],
        );
        assert_failed_with(&verdict(&equal, "test.html"), MATCH_PREFIX);
    }

    #[test]
    fn fuzzy_tolerance_admits_a_difference_inside_its_range_and_not_outside_it() {
        // #0f0 against #0e0 is a channel difference of 17 over a 20x20 box, 400 pixels.
        let root = suite(
            "fuzzy",
            &[
                (
                    "inside.html",
                    &format!(
                        r#"<meta name=fuzzy content="maxDifference=17;totalPixels=400"><link rel=match href="ref.html">{GREEN_BOX}"#
                    ),
                ),
                (
                    "outside.html",
                    &format!(
                        r#"<meta name=fuzzy content="16;400"><link rel=match href="ref.html">{GREEN_BOX}"#
                    ),
                ),
                (
                    "ref.html",
                    r#"<div style="width:20px;height:20px;background:#0e0"></div>"#,
                ),
            ],
        );
        assert!(matches!(
            verdict(&root, "inside.html"),
            Verdict::Passed { .. }
        ));
        assert_failed_with(&verdict(&root, "outside.html"), DIFFER_PREFIX);
    }

    #[test]
    fn text_is_declined_because_the_reference_render_paints_no_glyphs() {
        // Two different words would both be unpainted and so equal. That is a false pass,
        // so the page is declined instead of compared.
        let root = suite(
            "text",
            &[
                (
                    "test.html",
                    r#"<link rel=match href="ref.html"><p>hello</p>"#,
                ),
                ("ref.html", "<p>world</p>"),
            ],
        );
        // The raster stage reports `MissingGlyph` for the run, which is what declines it. The
        // explicit glyph-run check after it is the second line of defence for a run the raster
        // stage does not report.
        let text = assert_declined(
            &verdict(&root, "test.html"),
            SkipReason::MissingEngineCapability,
        );
        assert!(text.contains("raster"), "{text}");
    }

    #[test]
    fn canvas_and_iframes_are_declined_as_missing_capabilities() {
        let root = suite(
            "features",
            &[
                (
                    "canvas.html",
                    r#"<link rel=match href="ref.html"><canvas width=10 height=10></canvas>"#,
                ),
                (
                    "iframe.html",
                    r#"<link rel=match href="ref.html"><iframe width=10 height=10></iframe>"#,
                ),
                ("ref.html", ""),
            ],
        );
        assert_declined(
            &verdict(&root, "canvas.html"),
            SkipReason::MissingEngineCapability,
        );
        assert_declined(
            &verdict(&root, "iframe.html"),
            SkipReason::MissingEngineCapability,
        );
    }

    #[test]
    fn a_canvas_created_by_a_script_is_declined_too() {
        // The check runs on the final DOM, so a canvas a script adds is not missed.
        let root = suite(
            "dynamic-canvas",
            &[
                (
                    "test.html",
                    r#"<link rel=match href="ref.html"><script>document.body.appendChild(document.createElement("canvas"))</script>"#,
                ),
                ("ref.html", ""),
            ],
        );
        assert_declined(
            &verdict(&root, "test.html"),
            SkipReason::MissingEngineCapability,
        );
    }

    #[test]
    fn a_script_runs_before_the_page_is_compared() {
        let root = suite(
            "script",
            &[
                (
                    "test.html",
                    r##"<link rel=match href="ref.html"><div id=d style="width:20px;height:20px"></div><script>document.getElementById("d").style.background = "#0f0"</script>"##,
                ),
                ("ref.html", GREEN_BOX),
            ],
        );
        assert!(matches!(
            verdict(&root, "test.html"),
            Verdict::Passed { .. }
        ));
    }

    #[test]
    fn an_uncaught_error_in_a_reftest_fails_it_before_it_is_compared() {
        let root = suite(
            "reftest-error",
            &[
                (
                    "test.html",
                    r#"<link rel=match href="ref.html"><script>throw new Error("boom")</script>"#,
                ),
                ("ref.html", ""),
            ],
        );
        assert_failed_with(&verdict(&root, "test.html"), REFTEST_UNCAUGHT_PREFIX);
    }

    #[test]
    fn a_crash_test_passes_when_it_completes_and_fails_on_an_uncaught_error() {
        let root = suite(
            "crash",
            &[
                (
                    "crashtests/ok.html",
                    r#"<div>fine</div><script>var x = 1 + 1;</script>"#,
                ),
                (
                    "crashtests/throws.html",
                    r#"<script>throw new Error("boom")</script>"#,
                ),
            ],
        );
        assert!(matches!(
            verdict(&root, "crashtests/ok.html"),
            Verdict::Passed { .. }
        ));
        assert_failed_with(
            &verdict(&root, "crashtests/throws.html"),
            CRASHTEST_UNCAUGHT_PREFIX,
        );
        assert!(CRASHTEST_PANIC_PREFIX.starts_with("crashtest:"));
    }

    #[test]
    fn manual_tests_are_declined_as_not_automatable_even_with_a_reference() {
        let root = suite(
            "manual",
            &[
                (
                    "manual/test.html",
                    r#"<link rel=match href="../ref.html"><div></div>"#,
                ),
                ("ref.html", ""),
            ],
        );
        assert_declined(
            &verdict(&root, "manual/test.html"),
            SkipReason::NotAutomatable,
        );
    }

    #[test]
    fn a_reference_chain_and_a_reference_outside_the_suite_are_declined_not_approximated() {
        let root = suite(
            "refused",
            &[
                ("chain.html", r#"<link rel=match href="mid.html">"#),
                ("mid.html", r#"<link rel=match href="ref.html">"#),
                (
                    "outside.html",
                    r#"<link rel=match href="https://example.org/ref.html">"#,
                ),
                ("ref.html", ""),
            ],
        );
        assert_declined(
            &verdict(&root, "chain.html"),
            SkipReason::UnsupportedHarnessShape,
        );
        assert_declined(
            &verdict(&root, "outside.html"),
            SkipReason::UnsupportedHarnessShape,
        );
    }

    #[test]
    fn a_page_painted_in_one_colour_is_declined_because_it_shows_nothing_to_compare() {
        // Two blank pages are equal, and that equality measures nothing about content.
        let root = suite(
            "solid",
            &[
                (
                    "test.html",
                    r#"<link rel=match href="ref.html"><div></div>"#,
                ),
                ("ref.html", "<div></div>"),
            ],
        );
        let text = assert_declined(
            &verdict(&root, "test.html"),
            SkipReason::UnverifiedCapability,
        );
        assert!(text.contains("single colour"), "{text}");
    }

    #[test]
    fn form_controls_are_declined_because_their_appearance_is_not_painted() {
        let root = suite(
            "controls",
            &[
                (
                    "test.html",
                    r#"<link rel=match href="ref.html"><input type=date>"#,
                ),
                ("ref.html", GREEN_BOX),
            ],
        );
        let text = assert_declined(
            &verdict(&root, "test.html"),
            SkipReason::MissingEngineCapability,
        );
        assert!(text.contains("form control"), "{text}");
    }

    #[test]
    fn a_reference_page_is_a_reference_file_not_an_unverified_test() {
        let root = suite(
            "reference-file",
            &[
                ("test.html", r#"<link rel=match href="ref-only.html">"#),
                ("ref-only.html", "<div></div>"),
            ],
        );
        // The runner registers every walked file's targets before any row runs.
        super::register_reference_targets(&root, ["test.html", "ref-only.html"]);
        let text = assert_declined(&verdict(&root, "ref-only.html"), SkipReason::ReferenceFile);
        assert!(text.contains("reference page"), "{text}");
    }
}
