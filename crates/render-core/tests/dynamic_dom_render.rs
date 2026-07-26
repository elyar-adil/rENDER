use render_core::css::cascade::{CascadeInput, CascadeOrigin};
use render_core::css::computed::{ComputationLimits, PropertyRegistry, compute_document_styles};
use render_core::css::selector::{MatchContext, parse_selector_list, select_all};
use render_core::css::stylesheet::{StyleSheet, parse_stylesheet};
use render_core::dom::{Dom, MutationKind, NodeId, NodeKind};
use render_core::html::parse_document;
use render_core::layout::{
    FormattingLimits, LayoutOptions, SimpleTextMeasurer, build_formatting_tree,
    layout_formatting_tree,
};
use render_core::paint::{
    DisplayList, DisplayListBuilderOptions, ReferenceTextShaper, build_display_list,
};

fn render_existing_dom(dom: &Dom, sheet: &StyleSheet) -> DisplayList {
    let styles = compute_document_styles(
        dom,
        &[CascadeInput {
            sheet,
            origin: CascadeOrigin::Author,
        }],
        &PropertyRegistry::standard_baseline(),
        &ComputationLimits::default(),
        &MatchContext::default(),
    );
    let formatting = build_formatting_tree(dom, &styles, &FormattingLimits::default());
    let layout = layout_formatting_tree(
        dom,
        &formatting,
        &styles,
        LayoutOptions::default(),
        &SimpleTextMeasurer,
    );
    build_display_list(
        &layout.fragments,
        &formatting,
        &styles,
        DisplayListBuilderOptions::default(),
        &ReferenceTextShaper,
    )
    .list
}

fn first_match(dom: &Dom, selector: &str) -> NodeId {
    let selectors = parse_selector_list(selector).expect("test selector must parse");
    select_all(dom, dom.document(), &selectors, &MatchContext::default())[0]
}

#[test]
fn character_data_mutation_rebuilds_render_output_without_reparsing_html() {
    let mut parsed =
        parse_document("<!doctype html><html><body><p id='message'>short</p></body></html>");
    let sheet =
        parse_stylesheet("html, body, p { display: block; } p { width: 80px; color: #123456; }");
    let paragraph = first_match(&parsed.dom, "#message");
    let text = parsed
        .dom
        .children(paragraph)
        .expect("paragraph must have children")
        .iter()
        .copied()
        .find(|child| {
            matches!(
                parsed.dom.node(*child).map(render_core::dom::Node::kind),
                Some(NodeKind::Text(_))
            )
        })
        .expect("paragraph must have a text node");

    let old_revision = parsed.dom.revision();
    let old_display = render_existing_dom(&parsed.dom, &sheet);

    // This is the DOM binding operation a JavaScript engine will call. The
    // original HTML source is deliberately unavailable from this point on.
    parsed
        .dom
        .set_character_data(text, "a longer value that wraps onto several lines")
        .expect("text mutation must succeed");

    let mutations = parsed
        .dom
        .mutations_since(old_revision)
        .expect("mutation history must cover the previous render revision");
    assert_eq!(mutations.from_revision, old_revision);
    assert_eq!(mutations.to_revision, parsed.dom.revision());
    assert!(mutations.impact().affects_layout());
    assert!(mutations.impact().affects_paint());
    assert!(matches!(
        mutations.records.as_slice(),
        [record] if matches!(
            &record.kind,
            MutationKind::CharacterData { target, .. } if *target == text
        )
    ));

    let new_display = render_existing_dom(&parsed.dom, &sheet);
    let diff = new_display.diff(&old_display);

    assert_eq!(old_display.dom_revision, old_revision);
    assert_eq!(new_display.dom_revision, parsed.dom.revision());
    assert_eq!(diff.from_revision, old_revision);
    assert_eq!(diff.to_revision, parsed.dom.revision());
    assert!(!diff.full_repaint);
    assert!(
        !diff.changed.is_empty() || !diff.inserted.is_empty() || !diff.removed.is_empty(),
        "the changed and newly wrapped glyph runs must be visible in the display-list diff"
    );
    assert!(
        !diff.dirty_rects.is_empty(),
        "incremental painting needs explicit dirty geometry"
    );
}
