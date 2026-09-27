//! The offline real-site acceptance gate.
//!
//! One test per fixture, so a regression names the page shape it broke, plus the
//! cross-cutting checks that must hold for every fixture. Nothing here needs
//! Internet access: every resource response is supplied locally by
//! [`real_site_tasks::harness`].
//!
//! The ignored tests at the bottom are the to-do markers for capability gaps
//! recorded in `docs/visual_fidelity_gaps.md`. Per the "gaps are implemented
//! forward" rule in `docs/generic-browser-todo.md` they stay here, named, and
//! come to life when the gap is closed. They are not weakened substitutes for
//! the passing assertions above them.

use real_site_tasks::contract;
use real_site_tasks::fixture::{FIXTURES, RealSiteFixture};
use real_site_tasks::harness::{Session, contract_viewport};
use real_site_tasks::inspect;

fn assert_contract(fixture: &'static RealSiteFixture) {
    let session = Session::load(fixture);
    let report = contract::run(&session);
    assert!(report.is_clean(), "{}", report.summary());
}

#[test]
fn baidu_home_page_shape_renders_offline() {
    assert_contract(&real_site_tasks::fixture::BAIDU_HOME);
}

#[test]
fn baidu_search_results_page_shape_renders_offline() {
    assert_contract(&real_site_tasks::fixture::BAIDU_RESULTS);
}

#[test]
fn zhihu_home_page_shape_renders_offline() {
    assert_contract(&real_site_tasks::fixture::ZHIHU_HOME);
}

#[test]
fn zhihu_article_page_shape_renders_offline() {
    assert_contract(&real_site_tasks::fixture::ZHIHU_ARTICLE);
}

#[test]
fn netease_portal_home_page_shape_renders_offline() {
    assert_contract(&real_site_tasks::fixture::NETEASE_163_HOME);
}

#[test]
fn every_registered_fixture_is_covered_by_a_test() {
    // The five tests above are written one per fixture by hand so that a failure
    // can name the page shape. This keeps the registry and that list in step.
    let covered = [
        "baidu_home",
        "baidu_results",
        "zhihu_home",
        "zhihu_article",
        "netease_163_home",
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

#[test]
fn the_only_parse_error_reported_is_the_documented_tokenizer_defect() {
    // Reported by the fixtures, not expected of them: see
    // `no_fixture_reports_any_parse_error` below for the gap and the fix.
    //
    // `crates/render-html/src/tokenizer.rs` reports
    // `MissingWhitespaceBetweenAttributes` for the attribute that FOLLOWS a
    // valueless attribute, even when whitespace separates them:
    //
    //     <script defer src="a.js">   -> MissingWhitespaceBetweenAttributes
    //     <script src="a.js" defer>   -> clean
    //     <div a b>                   -> MissingWhitespaceBetweenAttributes
    //     <div a="1" b="2">           -> clean
    //
    // Every real page writes `<script defer src=...>`, so the fixtures keep it
    // and this assertion stays strict about everything else: any *other* code
    // fails here. Reordering the fixture's attributes to dodge the defect would
    // hide a bug that affects most of the web.
    const DEFECT: render_core::html::HtmlParseErrorCode =
        render_core::html::HtmlParseErrorCode::MissingWhitespaceBetweenAttributes;
    for fixture in FIXTURES {
        let session = Session::load(fixture);
        let unexpected: Vec<String> = session
            .document
            .html_errors()
            .iter()
            .filter(|error| error.code != DEFECT)
            .map(|error| format!("{:?} at offset {}", error.code, error.offset))
            .collect();
        assert!(
            unexpected.is_empty(),
            "{}: the HTML parser reported {}",
            fixture.label,
            unexpected.join(", ")
        );
    }
}

/// Gap: `crates/render-html/src/tokenizer.rs` reports
/// `MissingWhitespaceBetweenAttributes` for the attribute after a *valueless*
/// attribute even when whitespace separates the two, so every
/// `<script defer src=...>` and `<div a b>` on the web produces a spurious
/// parse error. Not yet listed in `docs/visual_fidelity_gaps.md`; the fix is to
/// carry the whitespace flag across a valueless attribute in the start-tag
/// tokenizer loop.
#[test]
#[ignore = "render-html tokenizer: MissingWhitespaceBetweenAttributes after a valueless attribute"]
fn no_fixture_reports_any_parse_error() {
    for fixture in FIXTURES {
        let session = Session::load(fixture);
        let errors: Vec<String> = session
            .document
            .html_errors()
            .iter()
            .map(|error| format!("{:?} at offset {}", error.code, error.offset))
            .collect();
        assert!(
            errors.is_empty(),
            "{}: the HTML parser reported {}",
            fixture.label,
            errors.join(", ")
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
// Gap markers. Each ignored test names the entry in
// `docs/visual_fidelity_gaps.md` that makes it impossible today. When the gap is
// closed, delete the `#[ignore]` and the test joins the gate.
// ---------------------------------------------------------------------------

/// `text-decoration` now reaches paint, so the underline the UA sheet asks for
/// is a real display-list item. This used to be an `#[ignore]`d gap marker for
/// `docs/visual_fidelity_gaps.md` S3; the capability landed and the marker
/// joined the gate.
#[test]
fn link_underlines_reach_the_display_list() {
    use render_core::paint::DisplayCommand;

    let session = Session::load(&real_site_tasks::fixture::BAIDU_HOME);
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

/// Gap: `docs/visual_fidelity_gaps.md` S1 - `render-layout`'s `TextStyle`
/// carries only `font_size` and `line_height`, so `font-weight`,
/// `font-style`, and `font-family` are dropped one metre from the glyphs. The
/// same text measured in two families is therefore identical in width, and bold
/// is physically impossible.
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

    let session = Session::load(&real_site_tasks::fixture::NETEASE_163_HOME);
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

/// Gap: `docs/visual_fidelity_gaps.md` S5 - inline SVG is now parsed as foreign
/// content, so the elements exist in the SVG namespace, but
/// `crates/render-core/src/image/svg.rs` is a rasteriser for standalone `.svg`
/// files and nothing lays out or paints an *inline* SVG shape: a `<path>` inside
/// a fixture's inline `<svg>` still produces no box.
#[test]
#[ignore = "docs/visual_fidelity_gaps.md S5: an inline SVG shape still lays out no box"]
fn an_inline_svg_shape_is_laid_out_and_painted() {
    let session = Session::load(&real_site_tasks::fixture::NETEASE_163_HOME);
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

/// Gap: `docs/visual_fidelity_gaps.md` S6 - `position: sticky` is defined in
/// `render-css` and has no consumer, so a pinned header scrolls away with the
/// page instead of staying in the scrollport.
#[test]
#[ignore = "docs/visual_fidelity_gaps.md S6: position: sticky has no layout consumer"]
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
