//! CSS Writing Modes 3 `direction` and UAX #9 bidirectional layout.
//!
//! The reference measurer charges 8px for every narrow character and 4px for
//! a space at the default 16px font, so every x-coordinate below is checkable
//! by hand. Text runs are returned in logical order; only their rectangles and
//! stored text reflect the visual result, which is what the painter consumes.

#![allow(clippy::float_cmp)]

use crate::PhysicalRect;
use crate::fragment::FragmentKind;
use crate::solver::LayoutOutput;

use super::tests::{find, pipeline};

const RESET: &str = "html, body { display:block; margin:0; padding:0; border:0 } \
                     p { display:block; margin:0; padding:0; width:200px; font-size:16px }";

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

fn run<'a>(runs: &'a [(String, PhysicalRect)], text: &str) -> &'a PhysicalRect {
    &runs
        .iter()
        .find(|(candidate, _)| candidate == text)
        .unwrap_or_else(|| panic!("missing text run {text:?} in {runs:?}"))
        .1
}

fn box_rect(layout: &LayoutOutput, source: render_dom::NodeId) -> PhysicalRect {
    layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(source))
        .unwrap_or_else(|| panic!("missing fragment for the requested box"))
        .rect
}

#[test]
fn a_style_boundary_inside_an_rtl_run_reorders_with_the_run() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>אב<span>גד</span></p></body>",
        &format!("{RESET} #p {{ direction:rtl }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    // Both halves are level 1 and therefore one visual run; the span boundary
    // must not stop the reversal.
    assert_eq!(run(&runs, "דג").origin.x, 168.0, "{runs:?}");
    assert_eq!(run(&runs, "בא").origin.x, 184.0, "{runs:?}");
}

#[test]
fn an_atomic_inline_is_one_object_in_an_rtl_run() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>אב<span id='box'></span></p></body>",
        &format!(
            "{RESET} #p {{ direction:rtl }} #box {{ display:inline-block; width:16px; height:10px }}"
        ),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].0, "בא", "{runs:?}");
    assert_eq!(runs[0].1.origin.x, 184.0, "{runs:?}");
    let box_fragment = box_rect(&layout, find(&output.dom, "#box"));
    assert_eq!(box_fragment.origin.x, 168.0, "{box_fragment:?}");
    assert_eq!(box_fragment.size.width, 16.0, "{box_fragment:?}");
}

#[test]
fn the_html_dir_attribute_selects_the_paragraph_direction() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='p' dir='rtl'>אבג</p></body>",
        &format!("{RESET} [dir=rtl] {{ direction:rtl }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].0, "גבא", "{runs:?}");
    assert_eq!(runs[0].1.origin.x, 176.0, "{runs:?}");
}

#[test]
fn css_direction_overrides_the_first_strong_character() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='rtl'>abc</p><p id='ltr'>אבג</p></body>",
        &format!("{RESET} #rtl {{ direction:rtl }} #ltr {{ direction:ltr; margin-top:20px }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    // An LTR-only text in an RTL paragraph stays LTR internally but the line
    // starts at the right.
    assert_eq!(run(&runs, "abc").origin.x, 176.0, "{runs:?}");
    // A Hebrew-only text in an LTR paragraph is still a level-1 run, so the
    // characters reverse, but the paragraph starts at the left.
    assert_eq!(run(&runs, "גבא").origin.x, 0.0, "{runs:?}");
}

#[test]
fn an_rtl_paragraph_reverses_a_hebrew_run_and_starts_at_the_right_edge() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>אבג</p></body>",
        &format!("{RESET} #p {{ direction:rtl }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 1, "{runs:?}");
    // The DOM text is logical; the fragment is in display order.
    assert_eq!(runs[0].0, "גבא", "{runs:?}");
    assert_eq!(runs[0].1.origin.x, 176.0, "{runs:?}");
    assert_eq!(runs[0].1.size.width, 24.0, "{runs:?}");
    assert_eq!(
        output
            .dom
            .children(find(&output.dom, "#p"))
            .and_then(|children| children.first().copied())
            .and_then(|text| output.dom.node(text))
            .and_then(|node| match node.kind() {
                render_dom::NodeKind::Text(value) => Some(value.clone()),
                _ => None,
            }),
        Some("אבג".to_owned()),
        "layout must not mutate the DOM text"
    );
}

#[test]
fn an_ltr_phrase_inside_an_rtl_paragraph_keeps_its_internal_order() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>אבג abc</p></body>",
        &format!("{RESET} #p {{ direction:rtl }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    // Logical order is Hebrew, neutral space, then the LTR run. The neutral
    // space resolves to the RTL level, so it reverses with the Hebrew run;
    // visual order is the LTR run at the left, then " גבא" at the right.
    assert_eq!(run(&runs, "abc").origin.x, 148.0, "{runs:?}");
    assert_eq!(run(&runs, "abc").size.width, 24.0, "{runs:?}");
    assert_eq!(run(&runs, " גבא").origin.x, 172.0, "{runs:?}");
    assert_eq!(run(&runs, " גבא").size.width, 28.0, "{runs:?}");
}

#[test]
fn european_numbers_stay_ltr_inside_an_rtl_paragraph() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>אבג 123</p></body>",
        &format!("{RESET} #p {{ direction:rtl }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    // The number run is level 2, so it keeps "123" and is placed at the left
    // of the line; the Hebrew run and its preceding neutral space are level
    // 1 and reversed together.
    assert_eq!(run(&runs, "123").origin.x, 148.0, "{runs:?}");
    assert_eq!(run(&runs, "123").size.width, 24.0, "{runs:?}");
    assert_eq!(run(&runs, " גבא").origin.x, 172.0, "{runs:?}");
    assert_eq!(run(&runs, " גבא").size.width, 28.0, "{runs:?}");
}

#[test]
fn an_ltr_paragraph_reverses_only_its_embedded_rtl_run() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>abc אבג def</p></body>",
        RESET,
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 3, "{runs:?}");
    assert_eq!(runs[0].0, "abc ", "{runs:?}");
    assert_eq!(runs[0].1.origin.x, 0.0, "{runs:?}");
    assert_eq!(runs[1].0, "גבא", "{runs:?}");
    assert_eq!(runs[1].1.origin.x, 28.0, "{runs:?}");
    assert_eq!(runs[2].0, " def", "{runs:?}");
    assert_eq!(runs[2].1.origin.x, 52.0, "{runs:?}");
}

#[test]
fn an_rtl_paragraph_wraps_at_the_same_logical_opportunity() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>אבג אבג</p></body>",
        &format!("{RESET} #p {{ direction:rtl; width:30px }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs.len(), 2, "{runs:?}");
    for (text, rect) in &runs {
        assert_eq!(text, "גבא", "{runs:?}");
        assert_eq!(rect.origin.x, 6.0, "{runs:?}");
        assert_eq!(rect.size.width, 24.0, "{runs:?}");
    }
    assert_eq!(runs[0].1.origin.y, 0.0, "{runs:?}");
    assert_eq!(runs[1].1.origin.y, 19.2, "{runs:?}");
}

#[test]
fn an_rtl_text_indent_shortens_the_right_start_edge() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>אבג</p></body>",
        &format!("{RESET} #p {{ direction:rtl; text-indent:40px }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    // The line box starts 40px in from the right, so a 24px run begins at
    // 200 - 40 - 24.
    assert_eq!(runs[0].1.origin.x, 136.0, "{runs:?}");
}

#[test]
fn text_align_end_in_an_rtl_paragraph_uses_the_left_edge() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>אבג</p></body>",
        &format!("{RESET} #p {{ direction:rtl; text-align:end }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].1.origin.x, 0.0, "{runs:?}");
}

#[test]
fn text_align_end_in_an_ltr_paragraph_uses_the_right_edge() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>abc</p></body>",
        &format!("{RESET} #p {{ text-align:end }}"),
        400.0,
    );
    let runs = text_runs(&layout);
    assert_eq!(runs[0].1.origin.x, 176.0, "{runs:?}");
}
