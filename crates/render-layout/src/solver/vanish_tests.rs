//! Boxes owed by a subtree the cascade switched off.
//!
//! These tests pin the invariant that a per-frame "this element produced no
//! box" report has to be read against: **an element's own `display` value
//! says nothing about whether a box was owed for it.** CSS Display 3 §2.1 and
//! CSS 2.1 §9.3 give `display: none` the property of generating no box *at
//! all* - and, because the box tree is built top down, no box for any
//! descendant either. So an element that kept a perfectly correct
//! `display: inline-block` while sitting inside a `display: none` container has
//! no box, and that is the specified outcome, not a collapse.
//!
//! The distinction matters because the two halves look identical from the
//! outside. An element whose `display` is right and whose box is missing
//! because an ancestor is hidden, and an element whose `display` is right and
//! whose box is missing because the solver dropped it, produce the same
//! observation when the only thing recorded is "non-`none` display, no box".
//! Distinguishing them is the difference between chasing a solver bug that
//! does not exist and closing a real one, so each test asserts *both* halves:
//! no box while the ancestor is hidden, and the declared box once it is not.

use std::collections::BTreeMap;

use render_css::cascade::{CascadeInput, CascadeOrigin};
use render_css::computed::{
    ComputationLimits, ComputedStyle, PropertyRegistry, compute_document_styles,
};
use render_css::selector::MatchContext;
use render_css::stylesheet::parse_stylesheet;
use render_dom::NodeId;
use render_html::parse_document;

use crate::fragment::{Fragment, FragmentKind};
use crate::tree::{FormattingLimits, build_formatting_tree};
use crate::{LayoutOptions, PhysicalSize, SimpleTextMeasurer, layout_formatting_tree};

/// The subset of the user-agent stylesheet these tests depend on. The engine
/// keeps the real sheet in the embedder, so a test that wants a `div` to be a
/// block box has to say so; stating it here keeps the difference between "the
/// user agent made this a block" and "the author asked for a block" visible in
/// the test rather than buried in a constant elsewhere.
const UA_FLOW: &str = "html, body, div, p, ul, li { display: block; margin: 0 }";

struct Laid {
    dom: render_dom::Dom,
    styles: BTreeMap<NodeId, ComputedStyle>,
    layout: crate::solver::LayoutOutput,
}

impl Laid {
    fn has_box(&self, selector: &str) -> bool {
        self.box_of(selector).is_some()
    }

    fn box_of(&self, selector: &str) -> Option<Fragment> {
        let node = crate::solver::tests::find(&self.dom, selector);
        self.layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(node))
            .cloned()
    }

    fn display_of(&self, selector: &str) -> String {
        let node = crate::solver::tests::find(&self.dom, selector);
        self.styles
            .get(&node)
            .and_then(|style| style.get("display"))
            .map_or_else(|| "block".to_owned(), |value| value.css_text().to_owned())
    }
}

fn lay_out(html: &str, author_css: &str, width: f32) -> Laid {
    let parsed = parse_document(html);
    let ua = parse_stylesheet(UA_FLOW);
    let author = parse_stylesheet(author_css);
    let styles = compute_document_styles(
        &parsed.dom,
        &[
            CascadeInput {
                sheet: &ua,
                origin: CascadeOrigin::UserAgent,
            },
            CascadeInput {
                sheet: &author,
                origin: CascadeOrigin::Author,
            },
        ],
        &PropertyRegistry::standard_baseline(),
        &ComputationLimits::default(),
        &MatchContext::default(),
    );
    let formatting = build_formatting_tree(&parsed.dom, &styles, &FormattingLimits::default());
    let layout = layout_formatting_tree(
        &parsed.dom,
        &formatting,
        &styles,
        LayoutOptions {
            viewport: PhysicalSize {
                width,
                height: 600.0,
            },
            ..LayoutOptions::default()
        },
        &SimpleTextMeasurer,
    );
    Laid {
        dom: parsed.dom,
        styles,
        layout,
    }
}

/// A navigation dropdown the author stylesheet keeps collapsed, holding an
/// icon the same stylesheet sizes and an item list with no author `display`
/// of its own. The shape is the one a real portal header uses: the panel is
/// hidden until a pointer enters it, and everything inside it is fully styled.
const PANEL: &str = "<!doctype html><body>\
     <div id='panel'><i class='icon'></i><ul class='items'><li class='item'>one</li></ul></div>\
     </body>";

const PANEL_HIDDEN: &str = "#panel { display: none } \
     .icon { display: inline-block; width: 14px; height: 14px; vertical-align: middle }";

#[test]
fn inline_block_inside_a_display_none_panel_owes_no_box() {
    let laid = lay_out(PANEL, PANEL_HIDDEN, 400.0);
    // The declaration really did match and really is `inline-block`; that is
    // the whole reason the observation is confusing. It is the *panel*, not
    // the icon, that removed the box.
    assert_eq!(laid.display_of(".icon"), "inline-block");
    assert_eq!(laid.display_of("#panel"), "none");
    // CSS Display 3 §2.1: `display: none` generates no box, and the subtree
    // below it is not in the box tree at all.
    assert!(
        !laid.has_box("#panel"),
        "a display:none panel generated a box"
    );
    assert!(
        !laid.has_box(".icon"),
        "an inline-block inside a display:none panel generated a box"
    );
    assert!(
        !laid.has_box(".item"),
        "a list item inside a display:none panel generated a box"
    );
}

#[test]
fn the_same_inline_block_gets_its_declared_box_once_the_panel_is_visible() {
    // The control for the test above. The only difference is one declaration
    // on the panel, so a difference in the icon's box can only be the panel's
    // `display`, never the icon's own styling.
    let laid = lay_out(
        PANEL,
        &format!("{PANEL_HIDDEN} #panel {{ display: block }}"),
        400.0,
    );
    assert!(laid.has_box("#panel"), "a visible panel has no box");
    let icon = laid
        .box_of(".icon")
        .expect("an inline-block inside a visible panel generates its box");
    // The declared size, not the fallback of a dropped box.
    assert!(
        (icon.rect.size.width - 14.0).abs() < f32::EPSILON,
        "icon width: {icon:?}"
    );
    assert!(
        (icon.rect.size.height - 14.0).abs() < f32::EPSILON,
        "icon height: {icon:?}"
    );
    assert!(
        matches!(icon.kind, FragmentKind::Box(_)),
        "an inline-block is a box, not a text run: {icon:?}"
    );
    // A `li` the user agent made a list item, inside a visible panel, is a box
    // too - the one that owes a box does get one.
    assert!(laid.has_box(".item"), "a visible list item has no box");
}

#[test]
fn a_block_level_box_inside_the_panel_is_owed_and_delivered() {
    // The same question asked of a `display: block` element, which is the
    // other value a matching rule is routinely misread as having been lost.
    const SPINNER: &str =
        "<!doctype html><body><div id='panel'><span class='loading'>wait</span></div></body>";
    const RULES: &str = ".loading { display: block }";

    let visible = lay_out(
        SPINNER,
        &format!("{RULES} #panel {{ display: block }}"),
        400.0,
    );
    let loading = visible
        .box_of(".loading")
        .expect("a display:block element inside a visible panel generates its box");
    assert!(
        matches!(loading.kind, FragmentKind::Box(_)),
        "a display:block element is a box, not a text run: {loading:?}"
    );

    // One declaration apart, the same element owes nothing - and the value the
    // report would have read is byte-identical in both runs.
    let hidden = lay_out(SPINNER, &format!("{RULES} {PANEL_HIDDEN}"), 400.0);
    assert_eq!(
        visible.display_of(".loading"),
        hidden.display_of(".loading")
    );
    assert_eq!(hidden.display_of(".loading"), "block");
    assert!(
        !hidden.has_box(".loading"),
        "a display:block element inside a display:none panel generated a box"
    );
    // The panel's own geometry is what changed, so neither run is vacuous.
    assert!(visible.has_box("#panel"));
    assert!(!hidden.has_box("#panel"));
}
