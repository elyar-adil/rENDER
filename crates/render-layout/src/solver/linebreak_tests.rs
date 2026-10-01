//! Tests for the line breaking the solver does with what `crate::linebreak`
//! computes, and for the intrinsic widths that depend on it.
//!
//! These are layout-level tests: they run a document through the whole pipeline
//! and read geometry out of the fragment tree. Every assertion is written from
//! what CSS 2.1, CSS Text 3 or UAX #14 says, so it keeps passing as the
//! implementation is refined and would have failed before the break-opportunity
//! computation existed.
//!
//! The reference measurer charges `font_size / 2` for an ordinary character,
//! `font_size / 4` for a space and `font_size` for a wide one, so at the
//! default 16px an `A` is 8, a space 4 and a Han ideograph 16. Every number
//! below is checkable by hand.

#![allow(clippy::float_cmp)]

use std::collections::BTreeMap;

use render_css::cascade::{CascadeInput, CascadeOrigin};
use render_css::computed::{
    ComputationLimits, ComputedStyle, ComputedValue, PropertyRegistry, compute_document_styles,
};
use render_css::selector::MatchContext;
use render_css::stylesheet::parse_stylesheet;
use render_dom::NodeId;
use render_html::parse_document;

use crate::PhysicalRect;
use crate::fragment::FragmentKind;
use crate::solver::LayoutOptions;
use crate::solver::LayoutOutput;
use crate::solver::SimpleTextMeasurer;
use crate::solver::layout_formatting_tree;
use crate::tree::{FormattingLimits, build_formatting_tree};

use super::tests::find;

/// Every box states its display explicitly, because the registry's initial
/// `display` is `inline` and this harness has no user-agent stylesheet to make a
/// `div` a block.
const RESET: &str = "html, body, div, p, table, tr, td { display: block; margin: 0; padding: 0 } \
                      table { display: table } tr { display: table-row } td { display: table-cell } \
                      span, b, em { display: inline }";

type Styles = BTreeMap<NodeId, ComputedStyle>;

fn pipeline(html: &str, css: &str, width: f32) -> (render_html::ParseOutput, Styles, LayoutOutput) {
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
        &SimpleTextMeasurer,
    );
    (output, styles, layout)
}

/// The text of a box, as the sequence of lines it was broken into, top to
/// bottom. One line per line box, in paint order, which for a single inline
/// formatting context is document order.
fn lines(layout: &LayoutOutput, source: NodeId) -> Vec<String> {
    let mut collected: Vec<(f32, f32, String)> = layout
        .fragments
        .iter()
        .filter(|fragment| fragment.source == Some(source))
        .filter_map(|fragment| match &fragment.kind {
            FragmentKind::Text(data) => Some((
                fragment.rect.origin.y,
                fragment.rect.origin.x,
                data.text.clone(),
            )),
            FragmentKind::Box(_) => None,
        })
        .collect();
    collected.sort_by(|left, right| left.0.total_cmp(&right.0).then(left.1.total_cmp(&right.1)));
    let mut lines: Vec<String> = Vec::new();
    let mut current: Option<f32> = None;
    for (y, _, text) in collected {
        if current != Some(y) {
            lines.push(String::new());
            current = Some(y);
        }
        lines
            .last_mut()
            .expect("a line was just pushed for the first fragment")
            .push_str(&text);
    }
    lines
}

fn box_rect(layout: &LayoutOutput, source: NodeId) -> PhysicalRect {
    layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(source))
        .unwrap_or_else(|| panic!("no fragment for the requested element"))
        .rect
}

#[test]
fn an_unspaced_ideograph_run_wraps_per_character() {
    // UAX #14 rule LB31 ("break everywhere else") gives an ideograph an
    // opportunity on both sides, so a Chinese paragraph fills the line
    // character by character instead of running off it.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p' style='width:48px'>\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}</p></body>",
        RESET,
        400.0,
    );
    let paragraph = find(&output.dom, "#p");
    let text = output.dom.children(paragraph).unwrap()[0];

    // Three ideographs fit in 48px at 16px each; the rest go to the next line.
    assert_eq!(
        lines(&layout, text),
        ["\u{4e00}\u{4e8c}\u{4e09}", "\u{56db}\u{4e94}"]
    );
}

#[test]
fn an_unspaced_ideograph_run_still_reports_a_wide_line_as_taller() {
    // The same paragraph with room for every character stays one line, which is
    // what makes the test above a wrap rather than a hard line.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p' style='width:80px'>\u{4e00}\u{4e8c}\u{4e09}</p></body>",
        RESET,
        400.0,
    );
    let paragraph = find(&output.dom, "#p");
    let text = output.dom.children(paragraph).unwrap()[0];

    assert_eq!(lines(&layout, text), ["\u{4e00}\u{4e8c}\u{4e09}"]);
    assert_eq!(box_rect(&layout, paragraph).size.height, 19.2);
}

#[test]
fn a_latin_word_still_overflows_rather_than_breaking() {
    // UAX #14 rule LB28: `(AL | HL) × (AL | HL)`, so "the" has no opportunity
    // inside it and a container narrower than the word overflows. This is the
    // Western half of the same computation the test above exercises.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p' style='width:20px'>the cat</p></body>",
        RESET,
        400.0,
    );
    let paragraph = find(&output.dom, "#p");
    let text = output.dom.children(paragraph).unwrap()[0];

    // "the" is 24px in a 20px box: it stays whole, and the break comes after it.
    assert_eq!(lines(&layout, text), ["the", "cat"]);
    let first = layout
        .fragments
        .iter()
        .filter(|fragment| fragment.source == Some(text))
        .map(|fragment| fragment.rect)
        .next()
        .expect("a text fragment");
    assert_eq!(first.size.width, 24.0);
}

#[test]
fn a_forbidden_line_start_class_moves_the_break_to_the_next_candidate() {
    // UAX #14 rule LB13: `× CL` keeps an ideographic comma with the character
    // before it, so a break before it is a non-break position and the break
    // after the preceding run is taken instead.
    //
    // Five ideographs and a comma in 48px: three ideographs fill a line, and
    // the fourth cannot be followed by a break that would put the comma at the
    // head of the next one, so the two travel together onto line two.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p' style='width:48px'>\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{3001}</p></body>",
        RESET,
        400.0,
    );
    let paragraph = find(&output.dom, "#p");
    let text = output.dom.children(paragraph).unwrap()[0];

    let lines = lines(&layout, text);
    assert_eq!(
        lines,
        ["\u{4e00}\u{4e8c}\u{4e09}", "\u{56db}\u{3001}"],
        "{lines:?}"
    );
    // The prohibition itself, stated as the property it exists for.
    assert!(
        !lines.iter().any(|line| line.starts_with('\u{3001}')),
        "a forbidden line-start class must not open a line: {lines:?}"
    );
}

#[test]
fn a_forbidden_line_end_class_moves_the_break_to_the_next_candidate() {
    // UAX #14 rule LB14: `OP SP* ×`, so a fullwidth opening bracket cannot end
    // a line. The break that would have landed after it is not available, so the
    // line ends before the bracket instead and the bracket keeps its partner.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p' style='width:32px'>\u{4e00}\u{4e8c}\u{ff08}\u{4e09}</p></body>",
        RESET,
        400.0,
    );
    let paragraph = find(&output.dom, "#p");
    let text = output.dom.children(paragraph).unwrap()[0];

    let lines = lines(&layout, text);
    assert_eq!(lines, ["\u{4e00}\u{4e8c}", "\u{ff08}\u{4e09}"], "{lines:?}");
    assert!(
        !lines.iter().any(|line| line.ends_with('\u{ff08}')),
        "a forbidden line-end class must not end a line: {lines:?}"
    );
}

#[test]
fn moving_a_break_does_not_change_the_measured_width_of_the_text() {
    // The requirement that makes kinsoku safe to implement as a narrowing of the
    // opportunity set: CSS Text 3 §7.2 puts half a letter's tracking on each
    // side of a character and §7.2 excludes the start and end of a line, so a
    // break that moves must not change any character's advance. The total of the
    // two lines is the width of the whole run either way.
    let text = "\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{3001}";
    let wide_line = format!("{RESET} #p {{ width: 400px }}");
    let (output, _, unwrapped) = pipeline(
        &format!("<!doctype html><body><p id='p'>{text}</p></body>"),
        &wide_line,
        400.0,
    );
    let paragraph = find(&output.dom, "#p");
    let source = output.dom.children(paragraph).unwrap()[0];

    let narrow = format!("{RESET} #p {{ width: 48px }}");
    let (wrapped_output, _, wrapped) = pipeline(
        &format!("<!doctype html><body><p id='p'>{text}</p></body>"),
        &narrow,
        400.0,
    );
    let wrapped_source = wrapped_output
        .dom
        .children(find(&wrapped_output.dom, "#p"))
        .unwrap()[0];

    let sum = |layout: &LayoutOutput, source| -> f32 {
        layout
            .fragments
            .iter()
            .filter(|fragment| fragment.source == Some(source))
            .map(|fragment| fragment.rect.size.width)
            .sum()
    };
    let unwrapped_width = sum(&unwrapped, source);
    let wrapped_width = sum(&wrapped, wrapped_source);

    // Five ideographs at 16px, one of which is the 16px comma.
    assert_eq!(unwrapped_width, 80.0);
    // The comma's advance is 16px like any ideograph's, and a break does not
    // halve tracking or reflow a glyph, so the total is unchanged.
    assert!(
        (wrapped_width - unwrapped_width).abs() < f32::EPSILON,
        "wrapping changed the measured width: {wrapped_width} vs {unwrapped_width}"
    );
}

#[test]
fn line_break_strict_moves_a_break_that_loose_does_not() {
    // CSS Text 3 §5.2: breaks before characters of line breaking class CJ are
    // "forbidden for normal and strict line breaking and allowed in loose". The
    // two therefore place the break at different positions, and nothing else
    // about the run changes.
    let document = "<!doctype html><body><p id='p' style='width:48px'>\u{4e00}\u{3041}\u{4e00}\u{3041}</p></body>";

    let (strict_output, _, strict) = pipeline(
        document,
        &format!("{RESET} #p {{ line-break: strict }}"),
        400.0,
    );
    let strict_text = strict_output
        .dom
        .children(find(&strict_output.dom, "#p"))
        .unwrap()[0];
    let strict_lines = lines(&strict, strict_text);
    assert!(
        !strict_lines.iter().any(|line| line.starts_with('\u{3041}')),
        "strict must not open a line with a small kana: {strict_lines:?}"
    );

    let (loose_output, _, loose) = pipeline(
        document,
        &format!("{RESET} #p {{ line-break: loose }}"),
        400.0,
    );
    let loose_text = loose_output
        .dom
        .children(find(&loose_output.dom, "#p"))
        .unwrap()[0];
    let loose_lines = lines(&loose, loose_text);
    assert!(
        loose_lines.iter().any(|line| line.starts_with('\u{3041}')),
        "loose must be able to open a line with a small kana: {loose_lines:?}"
    );
}

#[test]
fn word_break_keep_all_stops_an_ideograph_run_from_breaking() {
    // CSS Text 3 §5.1: "In this style, sequences of CJK characters do not
    // break." The same document that wrapped per character now overflows.
    let document = "<!doctype html><body><p id='p' style='width:48px'>\u{4e00}\u{4e8c}\u{4e09}\u{56db}</p></body>";
    let (normal_output, _, normal) = pipeline(document, RESET, 400.0);
    let normal_text = normal_output
        .dom
        .children(find(&normal_output.dom, "#p"))
        .unwrap()[0];
    assert_eq!(lines(&normal, normal_text).len(), 2);

    let (keep_output, _, keep) = pipeline(
        document,
        &format!("{RESET} #p {{ word-break: keep-all }}"),
        400.0,
    );
    let keep_text = keep_output
        .dom
        .children(find(&keep_output.dom, "#p"))
        .unwrap()[0];
    assert_eq!(
        lines(&keep, keep_text),
        ["\u{4e00}\u{4e8c}\u{4e09}\u{56db}"]
    );
}

#[test]
fn word_break_break_all_permits_a_break_inside_a_latin_word() {
    // CSS Text 3 §5.1: "in some styles of CJK typesetting, English words are
    // allowed to break between any two letters, rather than only at spaces or
    // hyphenation points; this can be enabled with word-break: break-all".
    let document = "<!doctype html><body><p id='p' style='width:60px'>understanding</p></body>";

    // UAX #14 rule LB28 gives the word no opportunity inside it, so it is one
    // unbreakable run and overflows.
    let (normal_output, _, normal) = pipeline(document, RESET, 400.0);
    let normal_text = normal_output
        .dom
        .children(find(&normal_output.dom, "#p"))
        .unwrap()[0];
    assert_eq!(lines(&normal, normal_text), ["understanding"]);

    let (all_output, _, all) = pipeline(
        document,
        &format!("{RESET} #p {{ word-break: break-all }}"),
        400.0,
    );
    let all_text = all_output
        .dom
        .children(find(&all_output.dom, "#p"))
        .unwrap()[0];
    // Seven characters at 8px fit in 60px, so the word is split there.
    assert_eq!(lines(&all, all_text), ["underst", "anding"]);
}

#[test]
fn white_space_pre_suppresses_wrapping_as_well_as_nowrap() {
    // CSS Text 3 §3: `pre` "does not allow wrapping", and §5: "When wrapping is
    // enabled (see white-space) ... by wrapping the line at a soft wrap
    // opportunity, if one exists." A value that forbids wrapping has no soft
    // wrap opportunity to wrap at, whatever the characters are.
    let document = "<!doctype html><body><p id='p' style='width:48px'>\u{4e00}\u{4e8c}\u{4e09}\u{56db}</p></body>";
    for white_space in ["pre", "nowrap"] {
        let (output, _, layout) = pipeline(
            document,
            &format!("{RESET} #p {{ white-space: {white_space} }}"),
            400.0,
        );
        let text = output.dom.children(find(&output.dom, "#p")).unwrap()[0];
        assert_eq!(
            lines(&layout, text),
            ["\u{4e00}\u{4e8c}\u{4e09}\u{56db}"],
            "white-space: {white_space} must not wrap"
        );
    }
}

#[test]
fn a_declaration_of_word_break_and_line_break_is_read_by_the_cascade() {
    // The two properties are registered, inherited and typed, so a descendant
    // of a `word-break: keep-all` element keeps it without declaring it. A
    // property with no registry entry is not in the computed style at all, which
    // is how `text-overflow` has to be read as raw token text.
    let (output, styles, _) = pipeline(
        "<!doctype html><body><div id='d' style='word-break:keep-all;line-break:strict'><p id='p'>\u{4e00}\u{4e8c}</p></div></body>",
        RESET,
        400.0,
    );
    let div = find(&output.dom, "#d");
    let paragraph = find(&output.dom, "#p");

    for node in [div, paragraph] {
        let style = &styles[&node];
        assert_eq!(
            style.get("word-break").map(ComputedValue::css_text),
            Some("keep-all"),
            "word-break must be in the computed style"
        );
        assert_eq!(
            style.get("line-break").map(ComputedValue::css_text),
            Some("strict"),
            "line-break must be in the computed style"
        );
    }
}

#[test]
fn an_invalid_line_break_value_falls_back_rather_than_being_read_as_a_keyword() {
    // `line-break: strictish` is not a keyword, so the grammar rejects it and
    // the element keeps the initial value. Reading the raw text instead would
    // make an unknown value behave like a real one.
    let (output, styles, _) = pipeline(
        "<!doctype html><body><p id='p' style='line-break:strictish'>\u{4e00}</p></body>",
        RESET,
        400.0,
    );
    let style = &styles[&find(&output.dom, "#p")];

    assert_eq!(
        style.get("line-break").map(ComputedValue::css_text),
        Some("auto")
    );
}

#[test]
fn a_float_holding_unspaced_text_is_as_wide_as_one_character() {
    // CSS 2.1 §10.3.5: a float's used width is the shrink-to-fit width,
    // `min(max(preferred minimum width, available width), preferred width)`.
    // With the whole paragraph available, the preferred width wins - so this
    // measures max-content and is the test that shows max-content is unchanged.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='f' style='float:left'>\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}</div></body>",
        RESET,
        400.0,
    );

    assert_eq!(box_rect(&layout, find(&output.dom, "#f")).size.width, 80.0);
}

#[test]
fn an_auto_table_column_holding_unspaced_text_reads_both_intrinsic_widths() {
    // CSS 2.1 §17.5.2.2: a cell's minimum width is its min-content width and its
    // maximum width is its max-content width, and an auto column is as wide as
    // its maximum. With the whole table available, the maximum wins - so the
    // column is as wide as the unspaced text, and the cell is one line tall
    // because at that width the text fits without a break.
    //
    // The minimum is what the *other* test pins: a table narrower than the text
    // must still lay it out one character per line rather than treating the cell
    // as unbreakable.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'><tr>\
           <td id='a'>\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}</td></tr></table></body>",
        &format!("{RESET} table {{ width: auto }}"),
        400.0,
    );

    assert_eq!(box_rect(&layout, find(&output.dom, "#t")).size.width, 80.0);
    assert_eq!(box_rect(&layout, find(&output.dom, "#a")).size.height, 19.2);
}

#[test]
fn an_auto_table_column_narrower_than_its_text_wraps_it_per_character() {
    // The consumer of min-content width: CSS 2.1 §17.5.2.2 gives an auto
    // column a minimum of the largest cell min-content width, and that minimum
    // is what stops a table from being narrower than its content.
    //
    // The cell's unbreakable run is one ideograph, so the minimum is 16px and a
    // 32px table is honoured. Were the minimum the whole paragraph - which is
    // what measuring min-content as "the widest whitespace-separated word" gives
    // for text with no whitespace - the minimum would be 80px and the table
    // would be forced wider than the `width` it was given.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='width:32px'><tr>\
           <td id='a'>\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}</td></tr></table></body>",
        &format!("{RESET} table {{ border-collapse: separate; border-spacing: 0 }}"),
        400.0,
    );
    let cell = find(&output.dom, "#a");
    let text = output.dom.children(cell).unwrap()[0];

    assert_eq!(
        box_rect(&layout, find(&output.dom, "#t")).size.width,
        32.0,
        "the table's declared width must be honoured"
    );
    // Two ideographs fit in 32px, so the five characters take three lines.
    let lines = lines(&layout, text);
    assert_eq!(lines.len(), 3, "{lines:?}");
    // `3 * 19.2` is not exact in binary floating point, so the comparison is a
    // tolerance rather than an equality the arithmetic cannot deliver.
    assert!(
        (box_rect(&layout, cell).size.height - 57.6).abs() < 0.01,
        "{:?} {lines:?}",
        box_rect(&layout, cell)
    );
}

#[test]
fn an_inline_block_holding_unspaced_text_still_reports_max_content() {
    // §10.3.9 gives an inline-block the shrink-to-fit width too, so the same
    // reasoning applies and the same helper answers it.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='row'><span id='ib' style='display:inline-block'>\u{4e00}\u{4e8c}\u{4e09}</span></p></body>",
        RESET,
        400.0,
    );

    assert_eq!(box_rect(&layout, find(&output.dom, "#ib")).size.width, 48.0);
}
