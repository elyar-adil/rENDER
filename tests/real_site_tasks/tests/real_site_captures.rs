//! Offline checks over the real-page captures.
//!
//! Each capture is a live page reduced in place. Its facts were measured from the
//! raw bytes, so a failure here means the engine's view of the page no longer
//! matches what the page says, not that a shape rule was broken. Nothing here
//! needs Internet access: every resource is answered by the harness.

use real_site_tasks::captures::{
    CAPTURES, Capture, GOOGLE_HOME, HACKER_NEWS_HOME, WIKIPEDIA_MAIN_PAGE, YAHOO_HK_HOME,
};
use render_core::document::AuthorStyleSource;
use render_core::script::ScriptSource;
use real_site_tasks::fixture::RealSiteFixture;
use real_site_tasks::harness::Session;
use real_site_tasks::inspect::{self, attribute, box_of, document_title, select, subtree_text};

/// Load a capture through the normal offline path. The fixture is leaked so the
/// harness's `'static` requirement holds; a test process loads a handful of
/// captures and nothing more.
fn load(capture: &Capture) -> Session {
    let fixture: &'static RealSiteFixture = Box::leak(Box::new(capture.as_fixture()));
    Session::load(fixture)
}

#[test]
fn every_capture_keeps_the_facts_measured_from_its_raw_page() {
    for capture in CAPTURES {
        let session = load(capture);
        let dom = session.document.dom();
        let label = capture.label;
        // The reduction may drop links; it may not add any.
        assert!(
            capture.anchors <= capture.raw_anchors,
            "{label}: the reduced fixture has more links than the raw page"
        );
        assert_eq!(
            document_title(dom).as_deref(),
            Some(capture.expected_title),
            "{label}: document title"
        );
        assert_eq!(
            select(dom, "a[href]").len(),
            capture.anchors,
            "{label}: a[href] count"
        );
        let linked = session
            .style_slots
            .iter()
            .filter(|slot| matches!(slot.source, AuthorStyleSource::External { .. }))
            .count();
        assert_eq!(linked, capture.stylesheets, "{label}: discovered stylesheet links");
        assert_eq!(
            session.external_style_slots().len(),
            capture.screen_stylesheets,
            "{label}: stylesheets that apply on screen"
        );
        // Inline scripts are discovered too; the raw page's `script[src]` count is
        // the external ones.
        let external_scripts = session
            .script_discovery
            .scripts
            .iter()
            .filter(|script| matches!(script.source, ScriptSource::External { .. }))
            .count();
        assert_eq!(
            external_scripts, capture.scripts,
            "{label}: discovered external scripts"
        );
        assert_eq!(
            select(dom, "input[name], textarea[name], select[name], button[name]").len(),
            capture.named_controls,
            "{label}: named controls"
        );
        assert_eq!(
            select(dom, "input[type=submit]").len(),
            capture.submit_inputs,
            "{label}: submit inputs"
        );
        assert_eq!(
            select(dom, "form[role=search]").len(),
            capture.search_forms,
            "{label}: search forms"
        );
        assert_eq!(
            session.image_discovery.resources.len(),
            capture.images_with_src,
            "{label}: image resources"
        );
    }
}

#[test]
fn hacker_news_story_table_keeps_its_rows_titles_and_reading_order() {
    let session = load(&HACKER_NEWS_HOME);
    let dom = session.document.dom();
    let fragments = session.fragments();

    let rows = select(dom, "tr.athing");
    assert_eq!(rows.len(), HACKER_NEWS_HOME.story_rows, "story rows");

    // Every story row is laid out, and the rows descend the page in document order.
    let mut previous_top = f32::NEG_INFINITY;
    for (index, row) in rows.iter().enumerate() {
        let laid_out = box_of(fragments, *row)
            .unwrap_or_else(|| panic!("story row {index} lays out to no box"));
        let top = laid_out.border.origin.y;
        assert!(
            laid_out.border.size.height > 0.0,
            "story row {index} has no height"
        );
        assert!(
            top > previous_top,
            "story row {index} at y={top} does not follow the row above it at y={previous_top}"
        );
        previous_top = top;
    }

    // The title links carry the raw page's text, first and last.
    let titles = select(dom, "span.titleline > a");
    assert!(
        titles.len() >= HACKER_NEWS_HOME.story_rows,
        "fewer title links ({}) than story rows",
        titles.len()
    );
    let first = subtree_text(dom, titles[0]);
    let last = subtree_text(dom, titles[HACKER_NEWS_HOME.story_rows - 1]);
    assert_eq!(
        Some(first.trim()),
        HACKER_NEWS_HOME.first_story_title,
        "first story title"
    );
    assert_eq!(
        Some(last.trim()),
        HACKER_NEWS_HOME.last_story_title,
        "last story title"
    );
}

#[test]
fn google_search_form_keeps_its_query_control_and_buttons_on_screen() {
    let session = load(&GOOGLE_HOME);
    let dom = session.document.dom();
    let fragments = session.fragments();

    let forms = select(dom, "form[role=search]");
    assert_eq!(forms.len(), GOOGLE_HOME.search_forms, "search forms");

    // The query is a textarea named q, and it lays out to a visible box.
    let query = select(dom, "textarea[name=q]");
    assert_eq!(query.len(), 1, "query textarea");
    let laid_out = box_of(fragments, query[0]).expect("the query textarea lays out to a box");
    assert!(
        laid_out.border.size.width > 0.0 && laid_out.border.size.height > 0.0,
        "the query textarea has no size: {:?}",
        laid_out.border.size
    );

    // The raw page carries each submit button twice: once in the form and once
    // in the autocomplete popup, which is hidden. Only the form's pair lays out,
    // and those are the buttons a user sees.
    let submits = select(dom, "input[type=submit]");
    assert_eq!(submits.len(), GOOGLE_HOME.submit_inputs, "submit inputs");
    let mut visible_labels: Vec<&str> = submits
        .iter()
        .filter(|submit| box_of(fragments, **submit).is_some())
        .filter_map(|submit| attribute(dom, *submit, "value"))
        .collect();
    visible_labels.sort_unstable();
    assert_eq!(
        visible_labels,
        ["Google Search", "I'm Feeling Lucky"],
        "the visible submit buttons"
    );
}

#[test]
fn every_capture_lays_out_visible_text() {
    // A capture that lays out no text is an empty page to the user, whatever its
    // markup says. This is the floor under every other check.
    for capture in CAPTURES.iter().filter(|capture| !capture.js_rendered_shell) {
        let session = load(capture);
        let lines = inspect::lines_in_reading_order(session.document.dom(), session.fragments());
        assert!(
            !lines.is_empty(),
            "{}: no text line was laid out",
            capture.label
        );
    }
}

#[test]
fn each_capture_search_input_lays_out_to_a_box() {
    // The search input each page's own form or header offers. A capture whose
    // search input has no box is a page the user cannot search. The YouTube shell
    // is not in this list: its search field is `hidden` in the static markup and
    // the page reveals it with its scripts.
    let search_inputs: [(&Capture, &str); 2] = [
        (&WIKIPEDIA_MAIN_PAGE, "input[name=search]"),
        (&YAHOO_HK_HOME, "input[name]"),
    ];
    for (capture, selector) in search_inputs {
        let session = load(capture);
        let dom = session.document.dom();
        let inputs = select(dom, selector);
        assert!(
            !inputs.is_empty(),
            "{}: no element matches {selector}",
            capture.label
        );
        let laid_out = inputs
            .iter()
            .any(|input| box_of(session.fragments(), *input).is_some_and(|boxed| boxed.border.size.width > 0.0));
        assert!(
            laid_out,
            "{}: none of the {} elements matching {selector} lays out to a box",
            capture.label,
            inputs.len()
        );
    }
}
