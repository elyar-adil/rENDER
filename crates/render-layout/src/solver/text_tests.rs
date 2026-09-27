//! Tests for the four text properties that had a computed value and no
//! consumer: `text-indent`, `letter-spacing`, `word-spacing` and
//! `text-transform`, plus `text-overflow: ellipsis`.
//!
//! Every assertion is concrete geometry from the reference measurer, which
//! charges `font_size / 2` for an ordinary character, `font_size / 4` for a
//! space and `font_size` for a wide one. At the default 16px that is 8, 4 and
//! 16, so every number below is checkable by hand.

#![allow(clippy::float_cmp)]

use std::collections::BTreeMap;

use render_css::cascade::{CascadeInput, CascadeOrigin};
use render_css::computed::{
    ComputationLimits, ComputedStyle, PropertyRegistry, compute_document_styles,
};
use render_css::selector::MatchContext;
use render_css::stylesheet::parse_stylesheet;
use render_dom::{Dom, NodeId, NodeKind};
use render_html::parse_document;

use crate::PhysicalRect;
use crate::fragment::FragmentKind;
use crate::solver::LayoutOptions;
use crate::solver::LayoutOutput;
use crate::solver::SimpleTextMeasurer;
use crate::solver::TextMeasurer;
use crate::solver::TextStyle;
use crate::solver::layout_formatting_tree;
use crate::tree::{FormattingLimits, build_formatting_tree};

use super::tests::find;

/// Every box in these fixtures states its display explicitly, because the
/// registry's initial `display` is `inline` and this harness has no user-agent
/// stylesheet to make a `div` a block.
const RESET: &str = "html, body, div, p, span { display: block; margin: 0; padding: 0 } \
                      span { display: inline }";

type Styles = BTreeMap<NodeId, ComputedStyle>;

/// The `tests` module's pipeline with the measurer left open, so a test can
/// prove a value reached `TextMeasurer` rather than only the painted result.
fn pipeline_measuring(
    html: &str,
    css: &str,
    width: f32,
    text_measurer: &dyn TextMeasurer,
) -> (render_html::ParseOutput, Styles, LayoutOutput) {
    let output = parse_document(html);
    let sheet = parse_stylesheet(css);
    let styles = compute_document_styles(
        &output.dom,
        &[CascadeInput {
            sheet: &sheet,
            origin: CascadeOrigin::Author,
        }],
        &PropertyRegistry::standard_baseline(),
        &ComputationLimits::default(),
        &MatchContext::default(),
    );
    let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
    let layout = layout_formatting_tree(
        &output.dom,
        &formatting,
        &styles,
        LayoutOptions {
            viewport: crate::PhysicalSize {
                width,
                height: 600.0,
            },
            ..LayoutOptions::default()
        },
        text_measurer,
    );
    (output, styles, layout)
}

fn pipeline(html: &str, css: &str, width: f32) -> (render_html::ParseOutput, Styles, LayoutOutput) {
    pipeline_measuring(html, css, width, &SimpleTextMeasurer)
}

/// The text fragments of a layout, in paint order, as `(text, rect)`.
fn text_runs(layout: &LayoutOutput) -> Vec<(String, PhysicalRect)> {
    layout
        .fragments
        .iter()
        .filter_map(|fragment| match &fragment.kind {
            FragmentKind::Text(data) => Some((data.text.clone(), fragment.rect)),
            FragmentKind::Box(_) => None,
        })
        .collect()
}

fn box_rect(layout: &LayoutOutput, source: NodeId) -> PhysicalRect {
    layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(source))
        .unwrap_or_else(|| panic!("no fragment for the requested element"))
        .rect
}

/// The DOM text a node's first text child holds, which is what
/// `textContent` would report.
fn dom_text(dom: &Dom, source: NodeId) -> String {
    dom.children(source)
        .unwrap_or_default()
        .iter()
        .find_map(|child| match dom.node(*child).map(render_dom::Node::kind) {
            Some(NodeKind::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no text child"))
}

fn text_of(runs: &[(String, PhysicalRect)], index: usize) -> &str {
    &runs
        .get(index)
        .unwrap_or_else(|| panic!("expected at least {} text runs, got {runs:?}", index + 1))
        .0
}

// ---------------------------------------------------------------- text-indent

#[test]
fn text_indent_moves_the_first_line_and_leaves_the_rest_at_the_content_edge() {
    // "aaa bbb" is 24 + 4 + 24 = 52 wide, so with a 100px box "aaa bbb" fits
    // and "ccc" wraps. The indent belongs to the first formatted line only
    // (CSS Text 3 §8.1), so line one starts at 40 and line two at 0.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>aaa bbb ccc</p></body>",
        &format!("{RESET} p {{ width: 100px; text-indent: 40px }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0].0, "aaa bbb");
    assert_eq!(runs[0].1.origin.x, 40.0, "{runs:?}");
    assert_eq!(runs[0].1.size.width, 52.0, "{runs:?}");
    assert_eq!(runs[1].0, "ccc");
    assert_eq!(runs[1].1.origin.x, 0.0, "{runs:?}");
    assert_eq!(runs[1].1.size.width, 24.0, "{runs:?}");
    // The indent shortens the first line rather than letting it use the room
    // it takes: 100 - 40 leaves 60, and "aaa bbb ccc" needs 76.
    let paragraph = box_rect(&layout, find(&output.dom, "#p"));
    assert_eq!(paragraph.size.height, 38.4, "{paragraph:?}");
}

#[test]
fn a_text_indent_percentage_resolves_against_the_blocks_own_inline_size() {
    // §8.1: a percentage is "a percentage of the block container's own
    // logical width", which is 200, not the 400px viewport.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>aaa bbb</p></body>",
        &format!("{RESET} p {{ width: 200px; text-indent: 10% }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].1.origin.x, 20.0, "{runs:?}");
    let _ = find(&output.dom, "#p");
}

#[test]
fn an_em_text_indent_resolves_against_the_elements_own_font_size() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>aaa</p></body>",
        &format!("{RESET} p {{ font-size: 40px; text-indent: 1em }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].1.origin.x, 40.0, "{runs:?}");
    let _ = find(&output.dom, "#p");
}

#[test]
fn only_the_first_childs_anonymous_block_is_indented() {
    // §8.1: "the first line of an anonymous block box is only affected if it
    // is the first child of its parent element". A container that already has
    // a block child must not indent the run of text that follows it.
    let (leading, _, layout) = pipeline(
        "<!doctype html><body><div id='d'>tail aaa</div></body>",
        &format!("{RESET} #d {{ width: 100px; text-indent: 40px }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].1.origin.x, 40.0, "first child: {runs:?}");
    let _ = find(&leading.dom, "#d");

    let (after_block, _, layout) = pipeline(
        "<!doctype html><body><div id='d'><div id='h'>h</div>tail aaa</div></body>",
        &format!("{RESET} #d {{ width: 100px; text-indent: 40px }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].0, "h", "{runs:?}");
    assert_eq!(
        runs[0].1.origin.x, 40.0,
        "the heading is still a first child"
    );
    assert_eq!(runs[1].0, "tail aaa", "{runs:?}");
    assert_eq!(
        runs[1].1.origin.x, 0.0,
        "an anonymous block that is not the first child is not indented: {runs:?}"
    );
    let _ = find(&after_block.dom, "#d");
}

#[test]
fn hanging_inverts_which_lines_the_indent_applies_to() {
    // §8.1 `hanging`: "Inverts which lines are affected", so the first line
    // hangs out into the margin and the body of the text is indented. In a
    // 60px box "aaa bbb" is 52 and "ccc" wraps.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>aaa bbb ccc</p></body>",
        &format!("{RESET} p {{ width: 60px; text-indent: 20px hanging }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0].0, "aaa bbb");
    assert_eq!(runs[0].1.origin.x, 0.0, "{runs:?}");
    assert_eq!(runs[1].0, "ccc");
    assert_eq!(runs[1].1.origin.x, 20.0, "{runs:?}");
    let _ = find(&output.dom, "#p");
}

#[test]
fn a_negative_text_indent_hangs_the_first_line_out() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>aaa bbb ccc</p></body>",
        &format!("{RESET} p {{ width: 50px; text-indent: -20px }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0].1.origin.x, -20.0, "{runs:?}");
    assert_eq!(runs[0].1.size.width, 52.0, "{runs:?}");
    assert_eq!(runs[1].1.origin.x, 0.0, "{runs:?}");
    let _ = find(&output.dom, "#p");
}

#[test]
fn each_line_indents_every_forced_line_but_not_every_wrapped_one() {
    // §8.1 `each-line`: "the first line of each block container and each line
    // after a forced line break (but not lines after a soft wrap break)".
    let (forced, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>aaa<br>bbb</p></body>",
        &format!("{RESET} p {{ width: 200px; text-indent: 20px each-line }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0].1.origin.x, 20.0, "{runs:?}");
    assert_eq!(runs[1].1.origin.x, 20.0, "{runs:?}");
    let _ = find(&forced.dom, "#p");

    let (wrapped, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>aaa bbb ccc</p></body>",
        &format!("{RESET} p {{ width: 60px; text-indent: 20px each-line }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0].0, "aaa", "{runs:?}");
    assert_eq!(runs[0].1.origin.x, 20.0, "{runs:?}");
    assert_eq!(
        runs[1].1.origin.x, 0.0,
        "a soft wrap is not a forced line break: {runs:?}"
    );
    let _ = find(&wrapped.dom, "#p");
}

// ------------------------------------------------------------ letter-spacing

#[test]
fn letter_spacing_widens_every_gap_and_nothing_else() {
    // CSS Text 3 §7.2: tracking goes between adjacent units, so a run of five
    // characters carries four gaps: 5 * 8 + 4 * 4 = 56, against 40 unspaced.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>abcde</p></body>",
        &format!("{RESET} p {{ width: 200px; letter-spacing: 4px }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].0, "abcde");
    assert_eq!(runs[0].1.size.width, 56.0, "{runs:?}");

    let (plain, _, plain_layout) = pipeline(
        "<!doctype html><body><p id='p'>abcde</p></body>",
        &format!("{RESET} p {{ width: 200px }}"),
        400.0,
    );
    let plain_runs = text_runs(&plain_layout);
    assert_eq!(
        plain_runs[0].1.size.width, 40.0,
        "the unspaced case has to stay at 40 for the comparison to mean anything"
    );
    assert_eq!(
        runs[0].1.size.width - plain_runs[0].1.size.width,
        16.0,
        "four gaps of 4px"
    );
    let _ = (find(&output.dom, "#p"), find(&plain.dom, "#p"));
}

#[test]
fn letter_spacing_is_not_inserted_at_the_start_or_end_of_a_line() {
    // §7.2: "except it is not applied at the beginning or end of a line, so
    // that text always fits flush with the edge of the block." In a 30px box
    // "aa" is 8 + (8 + 10) = 26: one gap. If the trailing gap were inserted the
    // run would be 36 and would no longer fit, so the width of both runs
    // proves it is not.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>aa bb</p></body>",
        &format!("{RESET} p {{ width: 30px; letter-spacing: 10px }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0].0, "aa");
    assert_eq!(runs[0].1.origin.x, 0.0, "{runs:?}");
    assert_eq!(runs[0].1.size.width, 26.0, "{runs:?}");
    assert_eq!(runs[1].0, "bb");
    assert_eq!(
        runs[1].1.origin.x, 0.0,
        "a wrapped line starts flush with the content edge: {runs:?}"
    );
    assert_eq!(runs[1].1.size.width, 26.0, "{runs:?}");
    let _ = find(&output.dom, "#p");
}

#[test]
fn the_gap_between_differently_tracked_inlines_is_their_average() {
    // §7.2: "When the value varies between adjacent inlines, the effective
    // spacing between adjacent letters of different inlines amounts to the
    // average between the two." The four gaps are 2, 4, 6 and 4, so
    // 6 * 8 + 16 = 64.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>aa<span id='s'>bb</span>cc</p></body>",
        &format!("{RESET} p {{ width: 200px; letter-spacing: 2px }} #s {{ letter-spacing: 6px }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 3, "{runs:?}");
    assert_eq!(runs[0].0, "aa");
    assert_eq!(runs[0].1.size.width, 18.0, "{runs:?}");
    assert_eq!(runs[1].0, "bb");
    assert_eq!(runs[1].1.size.width, 26.0, "{runs:?}");
    assert_eq!(runs[2].0, "cc");
    assert_eq!(runs[2].1.size.width, 22.0, "{runs:?}");
    let end = runs[2].1.right();
    assert_eq!(end, 66.0, "{runs:?}");
    let _ = find(&output.dom, "#p");
}

#[test]
fn letter_spacing_reaches_the_measurer_on_both_measurement_paths() {
    // The laid-out line is measured one character at a time by the solver, and
    // a shrink-to-fit width is measured by handing a whole run to
    // `TextMeasurer::measure_spaced`. Both have to come out at 56, or the box
    // the author sized and the line the painter draws disagree.
    //
    // A measurer that never opts into tracking therefore still has to be
    // handed the value, which is exactly what this one does: it implements
    // only `measure` and inherits `measure_spaced`.
    struct PlainBackend;

    impl TextMeasurer for PlainBackend {
        fn measure(&self, text: &str, style: TextStyle) -> crate::solver::TextMeasure {
            let advance = text
                .chars()
                .map(|character| {
                    if character.is_whitespace() {
                        style.font_size * 0.25
                    } else {
                        style.font_size * 0.5
                    }
                })
                .sum();
            crate::solver::TextMeasure {
                advance,
                ascent: style.font_size * 0.8,
                descent: style.font_size * 0.2,
            }
        }
    }

    for white_space in ["normal", "pre"] {
        let (spaced_dom, _, spaced) = pipeline_measuring(
            "<!doctype html><body><div id='d'>abcde</div></body>",
            &format!(
                "{RESET} #d {{ float: left; letter-spacing: 4px; white-space: {white_space} }}"
            ),
            400.0,
            &PlainBackend,
        );
        assert_eq!(
            box_rect(&spaced, find(&spaced_dom.dom, "#d")).size.width,
            56.0,
            "white-space: {white_space}"
        );

        let (plain_dom, _, plain) = pipeline_measuring(
            "<!doctype html><body><div id='d'>abcde</div></body>",
            &format!("{RESET} #d {{ float: left; white-space: {white_space} }}"),
            400.0,
            &PlainBackend,
        );
        assert_eq!(
            box_rect(&plain, find(&plain_dom.dom, "#d")).size.width,
            40.0,
            "white-space: {white_space}"
        );
    }
}

#[test]
fn negative_letter_spacing_tightens_the_line() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>abcde</p></body>",
        &format!("{RESET} p {{ width: 200px; letter-spacing: -3px }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].1.size.width, 40.0 - 12.0, "{runs:?}");
    let _ = find(&output.dom, "#p");
}

#[test]
fn letter_spacing_normal_computes_to_no_extra_spacing() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>abcde</p></body>",
        &format!("{RESET} p {{ width: 200px; letter-spacing: normal }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].1.size.width, 40.0, "{runs:?}");
    let _ = find(&output.dom, "#p");
}

#[test]
fn a_zero_letter_spacing_without_a_unit_is_a_length() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>abcde</p></body>",
        &format!("{RESET} p {{ width: 200px; letter-spacing: 0 }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].1.size.width, 40.0, "{runs:?}");
    let _ = find(&output.dom, "#p");
}

// -------------------------------------------------------------- word-spacing

#[test]
fn word_spacing_adds_one_advance_per_separator() {
    // §7.1: the extra space is applied to each word separator left in the
    // text, so "a b" is 8 + 4 + 8 = 20 unspaced and 20 + 6 = 26 with 6px.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='d'>a b</div></body>",
        &format!("{RESET} #d {{ float: left; word-spacing: 6px }}"),
        400.0,
    );
    assert_eq!(
        box_rect(&layout, find(&output.dom, "#d")).size.width,
        26.0,
        "one separator"
    );

    let (two, _, layout) = pipeline(
        "<!doctype html><body><div id='d'>a b c</div></body>",
        &format!("{RESET} #d {{ float: left; word-spacing: 6px }}"),
        400.0,
    );
    assert_eq!(
        box_rect(&layout, find(&two.dom, "#d")).size.width,
        26.0 + 18.0,
        "two separators"
    );
}

#[test]
fn word_spacing_and_letter_spacing_combine() {
    // "a b" is 8 + 4 + 8 = 20 of glyph advance, two tracking gaps of 4 and one
    // extra word advance of 6: 34. The word advance is on top of the tracking
    // the separator already gets as a character unit.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='d'>a b</div></body>",
        &format!("{RESET} #d {{ float: left; letter-spacing: 4px; word-spacing: 6px }}"),
        400.0,
    );
    assert_eq!(box_rect(&layout, find(&output.dom, "#d")).size.width, 34.0);
}

// ----------------------------------------------------------- text-transform

#[test]
fn text_transform_uppercase_changes_the_rendered_characters_and_the_geometry() {
    // `ß` uppercases to `SS`, so the rendered run is one character longer than
    // the author's: 7 * 8 = 56 against 6 * 8 = 48.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>straße</p></body>",
        &format!("{RESET} p {{ width: 200px; text-transform: uppercase }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(text_of(&runs, 0), "STRASSE");
    assert_eq!(runs[0].1.size.width, 56.0, "{runs:?}");

    let (plain, _, plain_layout) = pipeline(
        "<!doctype html><body><p id='p'>straße</p></body>",
        &format!("{RESET} p {{ width: 200px }}"),
        400.0,
    );
    let plain_runs = text_runs(&plain_layout);
    assert_eq!(text_of(&plain_runs, 0), "straße");
    assert_eq!(plain_runs[0].1.size.width, 48.0, "{plain_runs:?}");
    let _ = (find(&output.dom, "#p"), find(&plain.dom, "#p"));
}

#[test]
fn text_transform_lowercase_leaves_the_dom_alone_and_renders_lower() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>MiXeD</p></body>",
        &format!("{RESET} p {{ width: 200px; text-transform: lowercase }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(text_of(&runs, 0), "mixed");
    assert_eq!(
        dom_text(&output.dom, find(&output.dom, "#p")),
        "MiXeD",
        "CSS Text 3 §2.1: the property 'has no effect on the underlying content'"
    );
}

#[test]
fn text_transform_does_not_mutate_the_dom_for_any_casing_keyword() {
    // The acceptance requirement in one test: whatever the solver did, the DOM
    // still holds the author's characters, so `textContent` is unchanged.
    for (keyword, rendered) in [
        ("uppercase", "MIXED CASE"),
        ("lowercase", "mixed case"),
        // §2.1: `capitalize` "Puts the first typographic letter unit of each
        // word, if lowercase, in titlecase; other characters are unaffected."
        // The rest of the word is *not* lowercased as it was in CSS 2.1, so
        // the already-uppercase letters survive and the lowercase first letter
        // of the second word is titlecased.
        ("capitalize", "MiXeD CASE"),
        ("none", "MiXeD cASE"),
    ] {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><p id='p'>MiXeD cASE</p></body>",
            &format!("{RESET} p {{ width: 300px; text-transform: {keyword} }}"),
            400.0,
        );
        let runs = text_runs(&layout);
        assert_eq!(text_of(&runs, 0), rendered, "text-transform: {keyword}");
        assert_eq!(
            dom_text(&output.dom, find(&output.dom, "#p")),
            "MiXeD cASE",
            "text-transform: {keyword} must not reach the DOM"
        );
    }
}

#[test]
fn capitalize_spans_an_inline_boundary_but_not_a_block_boundary() {
    // §2.1.1: "Out-of-flow boxes and inline box boundaries must not introduce
    // a text-transform word boundary". So `hel` + `lo` is one word.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='d'><span>hel</span><span>lo</span></div></body>",
        &format!("{RESET} #d {{ text-transform: capitalize }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    // `hel` titlecases because it starts the word; `lo` does not, because §2.1.1
    // says the inline box boundary between them introduces no word boundary.
    assert_eq!(text_of(&runs, 0), "Hel", "{runs:?}");
    assert_eq!(text_of(&runs, 1), "lo", "{runs:?}");
    assert_eq!(runs[0].1.right(), runs[1].1.origin.x, "{runs:?}");
    // A block-level box opens a new line box, so it opens a new word.
    let (blocks, _, block_layout) = pipeline(
        "<!doctype html><body><div id='d'><p>hello</p><p>world</p></div></body>",
        &format!("{RESET} #d {{ text-transform: capitalize }}"),
        400.0,
    );
    let block_runs = text_runs(&block_layout);
    assert_eq!(text_of(&block_runs, 0), "Hello", "{block_runs:?}");
    assert_eq!(text_of(&block_runs, 1), "World", "{block_runs:?}");
    let _ = (find(&output.dom, "#d"), find(&blocks.dom, "#d"));
}

#[test]
fn capitalize_leaves_the_interior_of_a_word_and_its_non_letters_alone() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>eBay, iPhone 3</p></body>",
        &format!("{RESET} p {{ width: 300px; text-transform: capitalize }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(text_of(&runs, 0), "EBay, IPhone 3", "{runs:?}");
    let _ = find(&output.dom, "#p");
}

#[test]
fn text_transform_is_inherited_because_the_registry_says_so() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='d'><p id='p'>shout</p></div></body>",
        &format!("{RESET} #d {{ text-transform: uppercase }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(text_of(&runs, 0), "SHOUT", "{runs:?}");
    let _ = (find(&output.dom, "#d"), find(&output.dom, "#p"));
}

#[test]
fn text_transform_reaches_intrinsic_widths_not_just_the_painted_run() {
    // The transform happens where layout first reads the text, so a
    // shrink-to-fit box measures the transformed characters too. If it were a
    // paint-time change, this box would size itself from `straße` and the line
    // would overflow it.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='d'>straße</div></body>",
        &format!("{RESET} #d {{ float: left; text-transform: uppercase }}"),
        400.0,
    );
    assert_eq!(box_rect(&layout, find(&output.dom, "#d")).size.width, 56.0);
}

// --------------------------------------------------------- text-overflow

#[test]
fn text_overflow_ellipsis_replaces_the_clipped_text_with_one_glyph() {
    // 60px of content, 8px per character, and one 8px ellipsis reserved, so
    // six characters fit and "g" is the one that is dropped.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='d'>abcdefghijklmno</div></body>",
        &format!(
            "{RESET} #d {{ width: 60px; overflow: hidden; text-overflow: ellipsis; \
             white-space: nowrap }}"
        ),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0].0, "abcdef");
    assert_eq!(runs[0].1.origin.x, 0.0, "{runs:?}");
    assert_eq!(runs[0].1.size.width, 48.0, "{runs:?}");
    assert_eq!(runs[1].0, "\u{2026}");
    assert_eq!(runs[1].1.origin.x, 48.0, "{runs:?}");
    assert_eq!(runs[1].1.size.width, 8.0, "{runs:?}");
    assert_eq!(
        runs[1].1.right(),
        56.0,
        "inside the 60px content box: {runs:?}"
    );
    // Truncation is a rendering concern; the clipped characters are still the
    // document's text.
    assert_eq!(
        dom_text(&output.dom, find(&output.dom, "#d")),
        "abcdefghijklmno"
    );
}

#[test]
fn text_overflow_ellipsis_keeps_the_lines_after_the_clipped_one() {
    // `white-space: nowrap` suppresses soft wraps but not forced breaks, so
    // `nowrap` plus a `<br>` is a block whose first line clips and whose second
    // line still has to be laid out. Truncating must drop the clipped
    // characters, not everything after them.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='d'>abcdefghij<br>zz</div></body>",
        &format!(
            "{RESET} #d {{ width: 60px; overflow: hidden; text-overflow: ellipsis; \
             white-space: nowrap }}"
        ),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 3, "{runs:?}");
    assert_eq!(runs[0].0, "abcdef");
    assert_eq!(runs[1].0, "\u{2026}");
    assert_eq!(runs[2].0, "zz");
    // The ellipsis already ended the clipped line, so the `<br>` that follows
    // it does not open an empty third line box.
    assert_eq!(runs[2].1.origin.y, 19.2, "two line boxes: {runs:?}");
    assert_eq!(runs[2].1.origin.x, 0.0, "{runs:?}");
    assert_eq!(
        dom_text(&output.dom, find(&output.dom, "#d")),
        "abcdefghij",
        "clipping is a rendering concern: the clipped characters are still the \
         document's first text child"
    );
}

#[test]
fn text_overflow_ellipsis_needs_a_clipping_overflow() {
    // §3.1: the value "only applies to blocks with overflow other than
    // visible". Both of these overflow the 60px box and neither is truncated.
    for css in [
        "#d { width: 60px; text-overflow: ellipsis; white-space: nowrap }",
        "#d { width: 60px; overflow: visible; text-overflow: ellipsis; white-space: nowrap }",
    ] {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='d'>abcdefghijklmno</div></body>",
            &format!("{RESET} {css}"),
            400.0,
        );
        let runs = text_runs(&layout);
        assert_eq!(runs.len(), 1, "{css}: {runs:?}");
        assert_eq!(runs[0].0, "abcdefghijklmno", "{css}");
        assert_eq!(runs[0].1.size.width, 120.0, "{css}: {runs:?}");
        let _ = find(&output.dom, "#d");
    }
}

#[test]
fn text_overflow_ellipsis_does_not_truncate_a_line_that_wraps() {
    // The lines fit, so there is nothing to clip and nothing to replace. An
    // implementation that reserved ellipsis room unconditionally would lose
    // the last word.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='d'>aaa bbb ccc</div></body>",
        &format!("{RESET} #d {{ width: 60px; overflow: hidden; text-overflow: ellipsis }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 2, "{runs:?}");
    for run in &runs {
        assert!(!run.0.contains('\u{2026}'), "{runs:?}");
    }
    assert_eq!(runs[0].0, "aaa bbb", "{runs:?}");
    assert_eq!(runs[1].0, "ccc", "{runs:?}");
    let _ = find(&output.dom, "#d");
}

#[test]
fn text_overflow_ellipsis_is_not_inherited() {
    // It is a property of the block whose content overflows, so a child
    // paragraph keeps its own text in full.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='d'><p id='p'>abcdefghij</p></div></body>",
        &format!(
            "{RESET} #d {{ overflow: hidden; text-overflow: ellipsis; \
             white-space: nowrap }}"
        ),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].0, "abcdefghij", "{runs:?}");
    assert_eq!(runs[0].1.size.width, 80.0, "{runs:?}");
    let _ = (find(&output.dom, "#d"), find(&output.dom, "#p"));
}

#[test]
fn text_overflow_clip_leaves_the_text_alone() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='d'>abcdefghij</div></body>",
        &format!(
            "{RESET} #d {{ width: 60px; overflow: hidden; text-overflow: clip; \
             white-space: nowrap }}"
        ),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].0, "abcdefghij", "{runs:?}");
    let _ = find(&output.dom, "#d");
}
