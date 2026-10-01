//! The offline real-site acceptance gate.
//!
//! One test per fixture, so a regression names the page shape it broke, plus the
//! cross-cutting checks that must hold for every fixture, plus the diagnostic
//! expectations. Nothing here needs Internet access: every resource response is
//! supplied locally by [`real_site_tasks::harness`].
//!
//! ## The two rules that shape this file
//!
//! **A test must never assert a defect.** Every assertion here is one that will
//! still be correct after the gap it covers closes. Where a documented gap makes
//! the behaviour impossible, the marker lives behind an `#[ignore]` that names
//! the gap, and it is written so that deleting the attribute promotes it. Five
//! markers were inherited; two are retired, one is left alone on instruction,
//! and two remain - see the round report for the evidence on each.
//!
//! **A test must be able to fail.** Every check that loops over a collection
//! also reports how many items it saw, and the collection sizes are declared per
//! fixture. An ordering assertion over an empty collection is the failure mode
//! that has already bitten this project twice, and `min_ordered_lines` exists
//! specifically to make it impossible.

use std::collections::BTreeMap;

use real_site_tasks::contract;
use real_site_tasks::diagnostic_set::{self, Expectation};
use real_site_tasks::diagnostics::{Kind, Stage};
use real_site_tasks::fixture::{
    BAIDU_HOME, BAIDU_RESULTS, FIXTURES, FORM_HEAVY_SIGNIN, NETEASE_163_HOME, ORIGINAL_FIXTURES,
    REFERENCE_TWO_COLUMN, RealSiteFixture, SPEC_DATA_TABLE, STICKY_TOOLBAR, ZHIHU_ARTICLE,
    ZHIHU_HOME,
};
use real_site_tasks::harness::{Session, contract_viewport};
use real_site_tasks::inspect;

fn assert_contract(fixture: &'static RealSiteFixture) {
    let session = Session::load(fixture);
    let report = contract::run(&session);
    assert!(report.is_clean(), "{}", report.summary());
}

#[test]
fn baidu_home_page_shape_renders_offline() {
    assert_contract(&BAIDU_HOME);
}

#[test]
fn baidu_search_results_page_shape_renders_offline() {
    assert_contract(&BAIDU_RESULTS);
}

#[test]
fn zhihu_home_page_shape_renders_offline() {
    assert_contract(&ZHIHU_HOME);
}

#[test]
fn zhihu_article_page_shape_renders_offline() {
    assert_contract(&ZHIHU_ARTICLE);
}

#[test]
fn netease_portal_home_page_shape_renders_offline() {
    assert_contract(&NETEASE_163_HOME);
}

/// The fixture that made the per-column document-order scoping necessary: a main
/// column with a real side rail beside it, not below it.
#[test]
fn a_two_column_reference_page_renders_offline() {
    assert_contract(&REFERENCE_TWO_COLUMN);
}

/// Dense normative table: row groups, a caption, `colspan`/`rowspan` merged
/// cells, and two `colgroup`s. The table layout path is 34 unit tests deep and
/// had never seen a real table.
#[test]
fn a_dense_data_table_page_renders_offline() {
    assert_contract(&SPEC_DATA_TABLE);
}

/// A sticky top bar containing a nested horizontal scrollport, over several
/// screens of content. The two interact, and only a page like this reaches it.
#[test]
fn a_sticky_toolbar_over_a_nested_scroll_region_renders_offline() {
    assert_contract(&STICKY_TOOLBAR);
}

/// A sign-in page whose controls are in no form, in one form, and owned by a
/// form that is not their ancestor - including one whose owner only the HTML
/// parser's form element pointer can supply.
#[test]
fn a_form_heavy_page_renders_offline() {
    assert_contract(&FORM_HEAVY_SIGNIN);
}

#[test]
fn every_registered_fixture_is_covered_by_a_test() {
    // The tests above are written one per fixture by hand so that a failure can
    // name the page shape. This keeps the registry and that list in step.
    let covered = [
        "baidu_home",
        "baidu_results",
        "zhihu_home",
        "zhihu_article",
        "netease_163_home",
        "reference_two_column",
        "spec_data_table",
        "sticky_toolbar",
        "form_heavy_signin",
    ];
    let registered: Vec<&str> = FIXTURES.iter().map(|fixture| fixture.label).collect();
    assert_eq!(registered, covered);
}

#[test]
fn every_fixture_renders_reproducibly() {
    for fixture in FIXTURES {
        let first = Session::load(fixture);
        let second = Session::load(fixture);
        assert_eq!(
            first.output.layout.fragments, second.output.layout.fragments,
            "{}: two runs of the reference path produced different fragment trees",
            fixture.label
        );
        assert_eq!(
            real_site_tasks::shots::capture(&first),
            real_site_tasks::shots::capture(&second),
            "{}: two runs of the reference path produced different rasters",
            fixture.label
        );
        assert_eq!(
            first.diagnostics(),
            second.diagnostics(),
            "{}: two runs of the reference path reported different diagnostics",
            fixture.label
        );
    }
}

#[test]
fn every_fixture_lays_out_past_the_first_screen() {
    let first_screen = contract_viewport().height;
    for fixture in FIXTURES {
        let session = Session::load(fixture);
        let bottom = inspect::document_bottom(session.fragments());
        assert!(
            session.fragments().max_scroll_offset().y > 0.0,
            "{}: the document does not scroll",
            fixture.label
        );
        assert!(
            bottom > first_screen,
            "{}: layout ends at y={bottom}, inside the {first_screen}px first screen",
            fixture.label
        );
    }
}

/// Gap: `crates/render-html/src/tokenizer.rs` reported
/// `MissingWhitespaceBetweenAttributes` for the attribute after a *valueless*
/// attribute even when whitespace separated the two, so every
/// `<script defer src=...>` and `<div a b>` on the web produced a spurious parse
/// error. That defect is **fixed**, so the marker this replaces has been retired
/// and this is now a permanent assertion.
///
/// The previous state of this file kept a weaker companion assertion that allowed
/// exactly that one code and nothing else. That is the "assert a defect" pattern:
/// it would have gone on passing after the fix, and it would have failed the day
/// the tokenizer regressed *further* into something it did not name. Both are
/// gone. This asserts the defect's own code is never reported, which is exact and
/// which fails the day the tokenizer breaks that way again.
#[test]
fn no_fixture_reports_the_documented_tokenizer_defect() {
    const DEFECT: render_core::html::HtmlParseErrorCode =
        render_core::html::HtmlParseErrorCode::MissingWhitespaceBetweenAttributes;
    for fixture in FIXTURES {
        let session = Session::load(fixture);
        let errors: Vec<String> = session
            .document
            .html_errors()
            .iter()
            .filter(|error| error.code == DEFECT)
            .map(|error| format!("{:?} at offset {}", error.code, error.offset))
            .collect();
        assert!(
            errors.is_empty(),
            "{}: the tokenizer reported the fixed defect {} times: {}",
            fixture.label,
            errors.len(),
            errors.join(", ")
        );
    }
}

/// Every fixture's parse-error set, exactly.
///
/// The sign-in fixture carries a `form` element inside a `table`, which is
/// malformed markup that real pages ship and that the standard's own
/// tree-construction rules make an error case. The engine reports four
/// `UnexpectedToken`s there and none anywhere else on any fixture, and that is
/// pinned here rather than left to a blanket "no errors" that the malformed
/// region would have broken.
///
/// This is **not** an assertion that the errors are correct. It is an assertion
/// that the set is exactly what it is, so a change in what the engine reports is
/// a visible diff - and the *deeper* question of whether each of those four is
/// required by the standard is left open and named in the round report rather
/// than decided here.
#[test]
fn every_fixture_reports_exactly_the_parse_errors_it_should() {
    for fixture in FIXTURES {
        let session = Session::load(fixture);
        let mut reported: BTreeMap<String, usize> = BTreeMap::new();
        for error in session.document.html_errors() {
            *reported.entry(format!("{:?}", error.code)).or_default() += 1;
        }
        // Only the sign-in fixture carries the malformed `form`-inside-`table`
        // region, so it is the only one with anything to report. Every other
        // fixture must be clean, and that is a real assertion: the tokenizer
        // defect above used to fire on all of them.
        let expected: BTreeMap<String, usize> = if fixture.label == "form_heavy_signin" {
            BTreeMap::from([("UnexpectedToken".to_owned(), 4)])
        } else {
            BTreeMap::new()
        };
        assert_eq!(
            reported, expected,
            "{}: the HTML parser's parse-error set changed",
            fixture.label
        );
    }
}

#[test]
fn no_fixture_falls_into_quirks_mode() {
    // Every fixture declares `<!doctype html>`, so a document that does not is a
    // fixture bug rather than a page-shape property.
    // `docs/visual_fidelity_gaps.md` S7 records that quirks-mode CSS semantics
    // are not implemented, so silently losing the doctype would silently change
    // the box model underneath the whole contract.
    for fixture in FIXTURES {
        let session = Session::load(fixture);
        assert_eq!(
            session.document.quirks_mode(),
            render_core::html::QuirksMode::NoQuirks,
            "{}: the fixture lost its doctype",
            fixture.label
        );
        assert!(
            !session.style_diagnostics.iter().any(|diagnostic| {
                diagnostic.code
                    == render_core::document::DocumentDiagnosticCode::QuirksModeUnsupported
            }),
            "{}: the render reported quirks mode",
            fixture.label
        );
    }
}

#[test]
fn a_document_without_a_doctype_reports_the_documented_quirks_gap() {
    // The honest complement of the test above: the S7 gap is still a gap, and
    // the engine says so out loud rather than quietly applying standards-mode
    // box model rules to a quirks-mode page.
    let document = render_core::document::Document::parse("<html><body><p>x</p></body></html>");
    let output = document.render_reference(render_core::document::DocumentRenderOptions::default());
    assert!(
        output.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == render_core::document::DocumentDiagnosticCode::QuirksModeUnsupported
        }),
        "a doctype-less document must report the documented quirks-mode gap"
    );
}

// ---------------------------------------------------------------------------
// Diagnostics as a first-class output.
//
// The engine now emits real diagnostics - at-rules it parses and drops,
// `@supports` conditions it cannot answer, declarations it throws out of a
// block - and `render-browser` counts them without displaying them, because
// mapping each message to a `StylesheetDiagnosticCode` does not exist yet. This
// file is therefore the only place in the tree where they can be observed, and
// the expectations below are the honest per-fixture statement of what the engine
// does with a real page's stylesheet.
//
// The rule the expectations encode: a diagnostic appearing is correct, and a
// diagnostic going missing is the worst failure available. So each entry carries
// an exact count, and removal fails.
// ---------------------------------------------------------------------------

/// The stylesheet diagnostics each fixture must produce, and exactly how many.
///
/// The counts are per load, so they also pin the number of sheets: a fixture with
/// two external slots must report each at-rule **twice**, because the harness
/// serves the same deterministic bytes to both. A regression that parsed only
/// the first sheet, or collected only one sheet's diagnostics, shows up here as
/// a count that is half what it should be.
///
/// The five original fixtures deliberately write no `@font-face`, no
/// `@keyframes`, no `@supports` and no star hack, so they must report **nothing
/// at all**. That is the honest expectation for a hand-written fixture
/// stylesheet, and it is a real assertion: the `@font-face` blocks in a
/// production sheet are the whole reason this gate exists.
fn expected_diagnostics(fixture: &str) -> Option<Expectation> {
    /// The four unimplemented at-rules, one unknown at-rule, one undecidable
    /// `@supports` and the legacy star hack. The four new fixtures each declare
    /// exactly this, across two external sheets plus one embedded `<style>`, so
    /// everything in the external sheets is reported twice and the embedded
    /// sheet's error once.
    fn the_four_new_fixtures(coded: Vec<((Stage, String), usize)>) -> Expectation {
        Expectation {
            kinds: vec![
                // Four `@font-face` blocks, in each of two external sheets.
                (Kind::AtRuleNotEvaluated("font-face".to_owned()), 8),
                // `@keyframes` and its `-webkit-` spelling, once each per sheet.
                (Kind::AtRuleNotEvaluated("keyframes".to_owned()), 2),
                (Kind::AtRuleNotEvaluated("-webkit-keyframes".to_owned()), 2),
                // `@-moz-document` is in no specification, so it answers
                // `Unrecognised`, which is a different fact from "cannot do it
                // yet" and gets different words.
                (Kind::AtRuleUnknown("-moz-document".to_owned()), 2),
                // `at-rule(@font-face)` is `Undecidable`, never a bool: the block
                // is dropped *and* reported, and the reason names the crate that
                // has to answer it. The `@supports at-rule(@font-face)` block in
                // each fixture is exactly this case.
                (
                    Kind::SupportsNotAnswered("at-rule(@font-face)".to_owned()),
                    2,
                ),
                // The legacy star hack, in the *embedded* sheet only, so it is
                // reported once. This is the diagnostic whose loss costs a whole
                // declaration block, and it is the reason the two sources are
                // tracked separately.
                (
                    Kind::InvalidDeclaration(
                        "a declaration starts with a property name, and '*' is not one".to_owned(),
                    ),
                    1,
                ),
            ],
            coded,
            sources_must_differ: true,
        }
    }

    match fixture {
        // The reference page and the sticky-toolbar page report nothing outside
        // the stylesheet stage. The reference page is the cleanest of the four
        // and the control for the other three. The sticky-toolbar page's nested
        // scrollport and sticky chip are *not* diagnostics: a scrollport is a
        // box, not a report.
        "reference_two_column" | "sticky_toolbar" => Some(the_four_new_fixtures(Vec::new())),
        // The table page. `BlockInsideInline` 161 times is a real statement about
        // the engine's box tree on a normative table - a `caption`, a `colspan`
        // header and a `ul` inside a `td` - and it is **pinned rather than
        // suppressed**. The first version of this table listed the formatting
        // stage as "must be silent" and the measurement rejected it: demanding
        // silence would be demanding that the engine not report what it can see.
        // The count is a regression detector; a change in the table solver's box
        // tree shows up here as a diff.
        "spec_data_table" => Some(the_four_new_fixtures(vec![(
            (Stage::Formatting, "BlockInsideInline".to_owned()),
            161,
        )])),
        // The sign-in page. `BlockInsideInline` 9 times is the same statement as
        // above, from the label-and-field rows; the four `UnexpectedToken`s are
        // the malformed `form`-inside-`table` region, which
        // `every_fixture_reports_exactly_the_parse_errors_it_should` also pins.
        "form_heavy_signin" => Some(the_four_new_fixtures(vec![
            ((Stage::Formatting, "BlockInsideInline".to_owned()), 9),
            ((Stage::HtmlParse, "UnexpectedToken".to_owned()), 4),
        ])),
        // The five hand-written originals: nothing at all, at any stage. That is
        // the honest expectation for a fixture stylesheet with no `@font-face`,
        // no `@keyframes`, no `@supports` and no star hack in it.
        "baidu_home" | "baidu_results" | "zhihu_home" | "zhihu_article" | "netease_163_home" => {
            Some(Expectation {
                kinds: Vec::new(),
                coded: Vec::new(),
                sources_must_differ: false,
            })
        }
        _ => None,
    }
}

#[test]
fn every_fixture_reports_exactly_the_diagnostics_it_should() {
    for fixture in FIXTURES {
        let Some(expected) = expected_diagnostics(fixture.label) else {
            panic!("{}: no diagnostic expectation is declared", fixture.label);
        };
        let session = Session::load(fixture);
        let stream = session.diagnostics();
        let problems = diagnostic_set::differences(&stream, &expected);
        assert!(
            problems.is_empty(),
            "{label}: the diagnostic stream does not match the expected set\n\
             \n  reported:\n{reported}\n  differences:\n  - {problems}\n\
             \n  An at-rule the engine does not evaluate appearing here is correct behaviour and \
             the expected set should gain it. A diagnostic that has STOPPED appearing is a \
             defect, and it is the worse of the two: nothing downstream can notice a missing one.",
            label = fixture.label,
            reported = stream.describe(),
            problems = problems.join("\n  - "),
        );
    }
}

/// The two diagnostic sources must be distinguishable, not merely both walked.
///
/// Each new fixture puts its declaration-list error in the embedded `<style>` and
/// its at-rules in the external sheets. If a diagnostic could only be attributed
/// to "a stylesheet", then moving it between the two would be invisible - and
/// the ability to tell them apart is what makes a count of 1 (the embedded
/// sheet) mean something next to a count of 2 (one per external slot).
#[test]
fn the_embedded_and_external_sheets_report_different_diagnostics() {
    for fixture in FIXTURES {
        let Some(expected) = expected_diagnostics(fixture.label) else {
            continue;
        };
        if !expected.sources_must_differ {
            continue;
        }
        let session = Session::load(fixture);
        let (user_agent, author) = session.stylesheet_diagnostics_by_source();
        let (embedded, external): (Vec<_>, Vec<_>) = author.iter().partition(|entry| {
            entry
                .node
                .is_some_and(|node| inspect::tag(session.document.dom(), node) == Some("style"))
        });
        assert!(
            user_agent.is_empty(),
            "{}: the user-agent stylesheet reported {} diagnostics",
            fixture.label,
            user_agent.len()
        );
        assert!(
            !embedded.is_empty(),
            "{}: the embedded <style> reported no diagnostics, so the two sources cannot \
             be told apart",
            fixture.label
        );
        assert!(
            external.len() > embedded.len(),
            "{}: the external sheets reported {} diagnostics but the embedded one reported \
             {}; the two sources are indistinguishable",
            fixture.label,
            external.len(),
            embedded.len()
        );
    }
}

/// The per-kind counts, printed.
///
/// This is not decoration. The counts are the measurement the round report is
/// made of, and a gate that reports nothing is a gate nobody can check a claim
/// against. It runs as a test rather than a print for exactly one reason: if the
/// diagnostic stream ever becomes *unmeasurable* - a stage that stops being
/// collected, a kind that stops classifying - this fails, which is the one
/// failure mode that would make every other diagnostic assertion in this file
/// vacuous.
#[test]
fn the_diagnostic_counts_are_reported() {
    use std::fmt::Write as _;

    let mut report = String::new();
    for fixture in FIXTURES {
        let session = Session::load(fixture);
        let stream = session.diagnostics();
        let _ = writeln!(
            report,
            "=== {} : {} diagnostics",
            fixture.label,
            stream.len()
        );
        report.push_str(&stream.describe());
        report.push('\n');
    }
    println!("{report}");
    // The measurement is only real if the stream is non-empty: the reference
    // raster emits one `MissingGlyph` per glyph on every fixture, so an empty
    // stream anywhere would mean diagnostics are not being collected at all.
    for fixture in FIXTURES {
        let session = Session::load(fixture);
        assert!(
            !session.diagnostics().is_empty(),
            "{}: the diagnostic stream is empty, so the measurement above is vacuous",
            fixture.label
        );
    }
}

// ---------------------------------------------------------------------------
// Gap markers. Each ignored test names the entry in
// `docs/visual_fidelity_gaps.md` that makes it impossible today. When the gap is
// closed, delete the `#[ignore]` and the test joins the gate.
//
// Two of the five markers inherited from the previous round were **retired**:
// `no_fixture_reports_any_parse_error`, because the tokenizer defect it named was
// fixed, and it is now a permanent assertion above; and
// `a_sticky_header_stays_in_the_scrollport` is *narrowed* below, because the
// capability landed but the raster path does not consume the constraint.
// ---------------------------------------------------------------------------

/// `text-decoration` now reaches paint, so the underline the UA sheet asks for
/// is a real display-list item. This used to be an `#[ignore]`d gap marker for
/// `docs/visual_fidelity_gaps.md` S3; the capability landed and the marker
/// joined the gate.
#[test]
fn link_underlines_reach_the_display_list() {
    use render_core::paint::DisplayCommand;

    let session = Session::load(&BAIDU_HOME);
    let decorations = session
        .display_list()
        .items()
        .iter()
        .filter(|item| matches!(item.command, DisplayCommand::TextDecoration(_)))
        .count();
    let links = inspect::select(session.document.dom(), "nav a[href]").len();
    assert!(
        decorations >= links,
        "{links} navigation links produced {decorations} decoration display items"
    );
}

/// The same assertion on every fixture, not just the first.
///
/// The previous version read one fixture, so a page whose UA sheet did not
/// underline links - or whose links were laid out inline with no decoration -
/// would not have been noticed. Every fixture has a `nav`, so every one of them
/// should produce underlines, and a fixture that produces none is a defect in the
/// same capability.
#[test]
fn every_fixture_underlines_its_navigation_links() {
    use render_core::paint::DisplayCommand;

    for fixture in FIXTURES {
        let session = Session::load(fixture);
        let dom = session.document.dom();
        let decorations = session
            .display_list()
            .items()
            .iter()
            .filter(|item| matches!(item.command, DisplayCommand::TextDecoration(_)))
            .count();
        let links = inspect::select(dom, "nav a[href]").len();
        assert!(
            links > 0,
            "{}: the fixture has no navigation links, so this check would be vacuous",
            fixture.label
        );
        assert!(
            decorations >= links,
            "{}: {links} navigation links produced {decorations} decoration display items",
            fixture.label
        );
    }
}

/// Gap: `docs/visual_fidelity_gaps.md` S1 - `render-layout`'s `TextStyle`
/// carries only `font_size` and `line_height`, so `font-weight`,
/// `font-style`, and `font-family` are dropped one metre from the glyphs. The
/// same text measured in two families is therefore identical in width, and bold
/// is physically impossible.
///
/// **Left in place deliberately this round**, on instruction: a font axis is
/// landing in another crate right now, so the marker is not the thing to be
/// judging. It is recorded here rather than moved so that whoever lands the axis
/// has one place to look, and so that this round's report can say plainly that
/// the marker was *not* retired and why. Note that it currently *passes* when
/// run explicitly, which means the marker is now understating the capability -
/// see the round report.
#[test]
#[ignore = "docs/visual_fidelity_gaps.md S1: TextStyle carries no family, weight, or style"]
fn text_measurement_depends_on_the_requested_font_family() {
    let document = render_core::document::Document::parse(
        "<!doctype html><style>\
         #a { display:block; width:400px; font-family:sans-serif; font-size:32px }\
         #b { display:block; width:400px; font-family:monospace; font-size:32px }\
         </style><p id=a>Rendering 渲染</p><p id=b>Rendering 渲染</p>",
    );
    let output = document.render_reference(render_core::document::DocumentRenderOptions::default());
    let dom = document.dom();
    let width_of = |id: &str| {
        let node = inspect::select_one(dom, &format!("#{id}")).expect("the probe element exists");
        let lines = inspect::subtree_text_node_lines(dom, &output.layout.fragments, node);
        lines.first().map_or(0.0, |line| line.rect.size.width)
    };
    let (sans, mono) = (width_of("a"), width_of("b"));
    assert!(
        (sans - mono).abs() > 0.5,
        "sans-serif and monospace measured the same text at {sans} and {mono}"
    );
}

/// The namespace half of `docs/visual_fidelity_gaps.md` S5 has landed: an inline
/// `<svg>` and its children are parsed as foreign content. The geometry half
/// has not; see the ignored test below.
#[test]
fn inline_svg_is_parsed_as_foreign_content() {
    use render_core::dom::Namespace;

    let session = Session::load(&NETEASE_163_HOME);
    let dom = session.document.dom();
    let svg = inspect::select_one(dom, "svg").expect("the fixture has an inline SVG badge");
    assert_eq!(
        inspect::namespace(dom, svg),
        Some(Namespace::Svg),
        "an inline <svg> is not in the SVG namespace"
    );
    let paths = inspect::select(dom, "path");
    assert!(!paths.is_empty(), "the inline SVG has no path children");
    for path in &paths {
        assert_eq!(
            inspect::namespace(dom, *path),
            Some(Namespace::Svg),
            "an SVG child is not in the SVG namespace"
        );
    }
}

/// The same half, on the second fixture that carries an inline `svg`.
///
/// The sticky-toolbar page draws its brand as an inline `<svg>` with a `viewBox`
/// and no `width`/`height`, which is the shape that forces the viewBox fallback
/// for sizing. Parsing it is a separate capability from parsing the portal's
/// sized badge, and only one of the two was being checked.
#[test]
fn an_unsized_inline_svg_is_parsed_as_foreign_content() {
    use render_core::dom::Namespace;

    let session = Session::load(&STICKY_TOOLBAR);
    let dom = session.document.dom();
    let svg = inspect::select_one(dom, "svg").expect("the fixture has an inline SVG brand mark");
    assert_eq!(inspect::namespace(dom, svg), Some(Namespace::Svg));
    assert!(
        attribute_of(dom, svg, "viewBox").is_some(),
        "the fixture's inline SVG has no viewBox, so it is not the unsized shape"
    );
    assert!(
        attribute_of(dom, svg, "width").is_none() && attribute_of(dom, svg, "height").is_none(),
        "the fixture's inline SVG declares explicit dimensions, so it is not the unsized shape"
    );
    for path in inspect::select(dom, "path") {
        assert_eq!(inspect::namespace(dom, path), Some(Namespace::Svg));
    }
}

fn attribute_of(
    dom: &render_core::dom::Dom,
    node: render_core::dom::NodeId,
    name: &str,
) -> Option<String> {
    inspect::attribute(dom, node, name).map(str::to_owned)
}

/// Gap: `docs/visual_fidelity_gaps.md` S5 - inline SVG is now parsed as foreign
/// content and rasterised as an image resource, so the **shape** is drawn, but
/// nothing in `render-core`'s document pipeline runs
/// [`render_core::image::inline_svg::discover_inline_svgs`] or installs its
/// rasters. The `svg` element itself still lays out no box, so the raster has
/// nothing to paint into.
///
/// Measured this round: with discovery and install performed by the caller - the
/// documented API - the raster is produced and the badge's box is laid out at
/// 16x16 from its `width`/`height`. The gap is the missing *call*, not the
/// missing capability, and that is why the two halves are separated below.
#[test]
#[ignore = "docs/visual_fidelity_gaps.md S5: the document pipeline never runs inline-SVG discovery"]
fn an_inline_svg_shape_is_laid_out_and_painted() {
    let session = Session::load(&NETEASE_163_HOME);
    let dom = session.document.dom();
    let paths = inspect::select(dom, "path");
    assert!(!paths.is_empty(), "the inline SVG has no path children");
    for path in &paths {
        assert!(
            inspect::box_of(session.fragments(), *path).is_some(),
            "an inline SVG <path> laid out no box"
        );
    }
}

/// The half of S5 that *did* land, as a permanent assertion, and the
/// measurement that pins down exactly what is missing.
///
/// `discover_inline_svgs` and `InlineSvgDiscovery::install` are the documented
/// API for turning an inline `<svg>` into a paintable image resource. This calls
/// them the way a caller must, and asserts:
///
/// 1. the raster is produced, for the fixture's own inline `<svg>`;
/// 2. its intrinsic size comes from the `width`/`height` attributes;
/// 3. it installs under the `svg` element's own key, so the ordinary image
///    command will find it - which is what makes the missing call the only thing
///    standing between this and a painted shape.
///
/// A regression in any of the three fails here. The *absence of the call* is the
/// gap, and the ignored test above is where it is recorded; this test is what
/// makes the gap's size known rather than guessed.
#[test]
fn an_inline_svg_rasterises_when_the_caller_asks_for_one() {
    use render_core::image::{ImageLimits, inline_svg::discover_inline_svgs};

    let session = Session::load(&NETEASE_163_HOME);
    let dom = session.document.dom();
    let svg = inspect::select_one(dom, "svg").expect("the fixture has an inline SVG badge");

    let discovery = discover_inline_svgs(dom, ImageLimits::default());
    let rasters: Vec<_> = discovery
        .resources
        .iter()
        .filter(|raster| raster.owner == svg)
        .collect();
    assert_eq!(
        rasters.len(),
        1,
        "expected exactly one raster for the fixture's inline <svg>, found {}",
        rasters.len()
    );
    assert!(
        discovery.diagnostics.is_empty(),
        "inline SVG discovery reported {:?}",
        discovery.diagnostics
    );

    let raster = rasters[0];
    // The fixture's badge declares width=16 height=16.
    assert_eq!(
        raster.image.intrinsic_size(),
        (16, 16),
        "the raster's intrinsic size did not come from the svg's width/height attributes"
    );

    // It installs, and the display-list image lookup finds it under the svg's own
    // key - which is the whole reason the missing caller is the only gap left.
    let mut images = render_core::image::ImageResources::default();
    let (installed, diagnostics) = discovery.install(&mut images, ImageLimits::default());
    assert!(
        diagnostics.is_empty(),
        "installing the inline SVG rasters reported {diagnostics:?}"
    );
    assert_eq!(installed.len(), 1);
    assert!(
        images.get_for_node(svg).is_some(),
        "the installed raster is not reachable from the svg element, so no image command \
         could find it"
    );
}

/// Gap: `docs/visual_fidelity_gaps.md` S6 - `position: sticky` is now parsed,
/// resolved after layout, and recorded as a `StickyConstraint`, and the
/// **fragment rectangle is correctly the normal-flow one**. What does not exist
/// is a consumer: nothing in `render-core` or `render-browser` reads
/// `FragmentTree::sticky_constraint`, so no displacement is ever applied and a
/// pinned header scrolls away.
///
/// The previous marker asserted the *painted* result by rasterising at a scrolled
/// viewport origin. It was promoted this round as instructed and it **failed**,
/// which is the finding: the capability is half-landed, so the marker was narrowed
/// to the half that is missing rather than restored as it was. Splitting it in
/// two is what makes the size of the gap knowable.
#[test]
fn a_sticky_box_keeps_its_normal_flow_layout_position() {
    use render_core::document::DocumentRenderOptions;
    use render_core::layout::PhysicalSize;

    let document = render_core::document::Document::parse(
        "<!doctype html><style>\
         body { margin:0 }\
         #bar { position:sticky; top:0; display:block; width:400px; height:40px }\
         .row { display:block; width:400px; height:300px }\
         </style><div id=bar>bar</div>\
         <div class=row>1</div><div class=row>2</div><div class=row>3</div>",
    );
    let output = document.render_reference(DocumentRenderOptions {
        layout: render_core::layout::LayoutOptions {
            viewport: PhysicalSize {
                width: 800.0,
                height: 600.0,
            },
            ..render_core::layout::LayoutOptions::default()
        },
        ..DocumentRenderOptions::default()
    });
    let dom = document.dom();
    let bar = inspect::select_one(dom, "#bar").expect("the probe element exists");
    let fragments = &output.layout.fragments;

    let constraint = fragments
        .iter()
        .find(|fragment| fragment.source == Some(bar))
        .and_then(|fragment| fragments.sticky_constraint(fragment.id))
        .expect("a `position: sticky` box must have a constraint recorded");
    assert_eq!(
        constraint.insets.top,
        Some(0.0),
        "the top inset was not read"
    );
    assert_eq!(
        constraint.scrollport,
        PhysicalSize {
            width: 800.0,
            height: 600.0
        },
        "the constraint's scrollport is not the viewport"
    );
    // §4.1: sticky creates no stacking context and does not move the box during
    // layout, so the recorded rectangle is the un-displaced one. This is the
    // claim that is true today *and* after a consumer lands, which is what makes
    // it a contract rather than a description.
    let laid_out = inspect::box_of(fragments, bar).expect("the sticky box laid out no box");
    assert!(
        (constraint.margin_rect.origin.y - laid_out.border.origin.y).abs() < 0.5,
        "the constraint places the box at y={} but layout put it at y={}",
        constraint.margin_rect.origin.y,
        laid_out.border.origin.y
    );
    // And it is at the top of the document, i.e. genuinely un-displaced: a
    // displacement applied at layout time would have moved it up by the scroll
    // offset, which is zero here, so this is the floor rather than the ceiling.
    assert!(
        laid_out.border.origin.y.abs() < 0.5,
        "a sticky box at the top of the document should lay out at y=0, laid out at y={}",
        laid_out.border.origin.y
    );
}

/// The half of S6 that is missing: **no consumer applies the displacement**.
///
/// This is the marker that stayed `#[ignore]`d, narrowed. It rasterises the same
/// page at a scrolled viewport origin and requires the bar still to be there.
/// Run explicitly this round it fails - the bar is gone from the raster - which
/// is the finding, and it is why the marker names the missing call rather than
/// the missing capability.
///
/// It asserts a *pixel*, which is unusual here, and the rule about never
/// asserting a defect is respected: this is a post-gap assertion. Once a
/// consumer exists it will be true, and it is not a baseline - it is a single
/// known colour on a known background, not a comparison against a stored image.
#[test]
#[ignore = "docs/visual_fidelity_gaps.md S6: nothing reads FragmentTree::sticky_constraint"]
fn a_sticky_header_stays_in_the_scrollport() {
    use render_core::document::DocumentRenderOptions;
    use render_core::layout::PhysicalPoint;
    use render_core::paint::{
        Color, CpuRasterizer, NoGlyphMasks, NoRasterCancellation, PaintScene, RasterRequest,
    };

    let document = render_core::document::Document::parse(
        "<!doctype html><style>\
         #bar { position:sticky; top:0; display:block; width:400px; height:40px; background-color:#ff0000 }\
         .row { display:block; width:400px; height:300px }\
         </style><div id=bar>bar</div>\
         <div class=row>1</div><div class=row>2</div><div class=row>3</div>",
    );
    let output = document.render_reference(DocumentRenderOptions::default());
    let scene = PaintScene::from_display_list(output.display.list.clone());
    let raster = CpuRasterizer
        .rasterize_request(
            RasterRequest::new(&scene, Color::rgb(255, 255, 255), &NoGlyphMasks)
                .with_viewport_origin(PhysicalPoint { x: 0.0, y: 500.0 }),
            &NoRasterCancellation,
        )
        .expect("a reference raster request is never cancelled");
    assert_eq!(
        raster.surface.pixel(200, 2),
        Some(Color::rgb(255, 0, 0)),
        "the sticky bar scrolled out of the viewport instead of staying pinned"
    );
}

/// Gap, found by the sticky-toolbar fixture: a sticky box inside a **nested**
/// scrollport resolves against the wrong scrollport.
///
/// The layout code uses `options.viewport` for *every* sticky box
/// (`crates/render-layout/src/solver/mod.rs`), so a sticky element inside a
/// scrollport gets the page's scrollport rather than the one it is in. The
/// recorded constraint therefore says the box may be displaced across the whole
/// page, when the specification says it is constrained to the scrollport it is
/// in. The layout code records this as a known dependency rather than papering
/// over it; this marker is the observable side of that.
///
/// Asserted against the fixture, because the fixture is the only thing that
/// contains the shape: a real navbar that is its own horizontal scroller, with
/// its first chip sticky against it.
#[test]
#[ignore = "docs/visual_fidelity_gaps.md S6: a sticky box in a nested scrollport uses the page scrollport"]
fn a_sticky_box_in_a_nested_scrollport_resolves_against_that_scrollport() {
    use render_core::layout::PhysicalSize;

    let session = Session::load(&STICKY_TOOLBAR);
    let dom = session.document.dom();
    let fragments = session.fragments();

    let chip = inspect::select_one(dom, ".tab-sticky").expect("the fixture has a sticky chip");
    let chip_box = fragments
        .iter()
        .find(|fragment| fragment.source == Some(chip))
        .expect("the sticky chip laid out no fragment");
    let constraint = fragments
        .sticky_constraint(chip_box.id)
        .expect("the sticky chip has no constraint recorded");

    // What it should be: the chip's own scrollport.
    let scroller = inspect::select_one(dom, ".navbar-scroll").expect("the fixture has a scroller");
    let scroller_fragment = fragments
        .iter()
        .find(|fragment| fragment.source == Some(scroller))
        .expect("the scroller laid out no fragment");
    let scrollport = fragments
        .scrollport(scroller_fragment.id)
        .expect("the scroller has no scrollport geometry");
    let inner = PhysicalSize {
        width: scrollport.clip.size.width,
        height: scrollport.clip.size.height,
    };

    // The chip is `position: sticky; left: 0` *inside* the scroller, so its
    // displacement is constrained horizontally by that scrollport. The page
    // scrollport would be a different size, and using it is the defect.
    assert_eq!(
        constraint.scrollport, inner,
        "a sticky box inside a nested scrollport is constrained by {:?}, which is not the \
         scrollport it is in",
        constraint.scrollport
    );
}

/// Gap, found by the two-column fixture: a rail can be laid out in a column
/// that overlaps the main column, or below it, and every other check still
/// passes.
///
/// The previous `ordered_scroll_region` check only ever saw single-column pages,
/// so nothing in the harness could have noticed. This marker is retained for the
/// case the fixture *does* pin - see `contract::column_shapes` for the passing
/// one - and covers the specific half that is not implemented: multi-column
/// layout. A page that asked for `columns: 2` would be laid out as one column,
/// and this marker says so.
///
/// Note that it uses `columns` as the *declaration*, not as the shape: the
/// fixture's own two columns come from flex, and a flex row is a correct answer
/// that no gap marker should be asserting against.
#[test]
#[ignore = "docs/visual_fidelity_gaps.md S6: multi-column layout is not implemented"]
fn a_multi_column_property_produces_more_than_one_column() {
    let document = render_core::document::Document::parse(
        "<!doctype html><style>\
         #a { display:block; width:400px; height:100px; columns:2; column-gap:20px }\
         #b { display:block; width:400px; height:100px }\
         </style><div id=a>1<br>2<br>3<br>4<br>5<br>6<br>7<br>8</div><div id=b>below</div>",
    );
    let output = document.render_reference(render_core::document::DocumentRenderOptions::default());
    let dom = document.dom();
    let a = inspect::select_one(dom, "#a").expect("the probe element exists");
    let b = inspect::select_one(dom, "#b").expect("the probe element exists");
    let lines = inspect::subtree_text_node_lines(dom, &output.layout.fragments, a);
    let columns: std::collections::BTreeSet<u32> = lines
        .iter()
        .map(|line| line.rect.origin.x.to_bits())
        .collect();
    assert!(
        columns.len() >= 2,
        "`columns: 2` produced {} distinct x positions, so the content is still one column",
        columns.len()
    );
    let a_box = inspect::box_of(&output.layout.fragments, a).expect("no box");
    inspect::box_of(&output.layout.fragments, b).expect("no box");
    assert!(
        a_box.border.size.height < 200.0,
        "the block with `columns: 2` is {} tall, which is more than the 100px of one column, \
         so the content was not distributed",
        a_box.border.size.height
    );
}

/// This is a **tracking** assertion, not a support assertion. It was originally `#[ignore]`d
/// against S4/S9 because the registry had no entry for web fonts or animations, so the gaps
/// could not be tracked or reported as progress. Both are now registered - `css.font-face`
/// and `css.animations`, both `SupportStatus::Missing`, which is the truth: `@font-face` is
/// parsed and discarded, and `@keyframes` likewise. Registering them as `Missing` rather than
/// editing this test is what keeps the gate honest.
///
/// The promotion from `#[ignore]` to a permanent assertion is deliberate. A test that asserts
/// "the gap is registered" keeps failing the day someone fixes the gap without updating the
/// registry, which is the direction of failure worth having.
#[test]
fn the_capability_registry_declares_web_font_and_animation_support() {
    use render_core::spec::FeatureRegistry;

    let registry = FeatureRegistry::current();
    let declares = |needle: &str| {
        registry
            .iter()
            .any(|definition| definition.id.as_str().contains(needle))
    };
    assert!(declares("font"), "no @font-face feature is registered");
    assert!(declares("animation"), "no animation feature is registered");
}

/// The same, for the two capabilities that landed this session and that the new
/// fixtures exist to cover. Registering them is what lets a gap be tracked; not
/// registering one is how a half-landed capability goes unnoticed, which is
/// exactly what happened to `position: sticky` and inline SVG.
///
/// The direction of failure is the point: this fails the day someone implements
/// one of them *without* updating the registry, which is the moment the "Closed"
/// table needs a re-read.
#[test]
fn the_capability_registry_declares_table_sticky_and_inline_svg_support() {
    use render_core::spec::FeatureRegistry;

    let registry = FeatureRegistry::current();
    let declares = |needle: &str| {
        registry
            .iter()
            .any(|definition| definition.id.as_str().contains(needle))
    };
    for (id, needle) in [
        ("css.tables", "table"),
        ("rendering.layout", "layout"),
        ("svg.inline-rasterization", "inline-rasterization"),
        ("html.forms", "forms"),
    ] {
        assert!(
            declares(needle),
            "no `{id}` feature is registered, so nothing tracks whether it is implemented"
        );
    }
}

/// The original five are still in the gate, in the order the acceptance document
/// lists them.
///
/// Added because adding four fixtures to the same list is exactly the kind of
/// change that quietly reorders the old ones. The assertion is about the list's
/// shape rather than its contents, so it survives the next addition.
#[test]
fn the_original_five_fixtures_are_still_in_the_gate() {
    assert_eq!(ORIGINAL_FIXTURES.len(), 5);
    for label in ORIGINAL_FIXTURES {
        assert!(
            FIXTURES.iter().any(|fixture| fixture.label == *label),
            "{label} is no longer in the fixture registry"
        );
    }
}
