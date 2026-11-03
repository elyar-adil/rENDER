#![allow(clippy::float_cmp)]

use crate::PhysicalRect;
use crate::fragment::FragmentKind;

use super::tests::{find, fragment, pipeline};

/// CSS 2.1 §10.8.1: an inline-block anchor with `line-height: 16px` and
/// `height: 16px` puts its `vertical-align: top` children on the line's top
/// edge. The weather strip in the portal fixture uses exactly this shape; when
/// the declaration was ignored the children were baseline-aligned to the
/// bottom edge of the anchor and the row looked split.
#[test]
fn weather_strip_children_align_to_the_inline_block_anchor_top() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='row'>\
           <a id='day'><span id='tag'>今</span><span id='text'>晴</span></a>\
         </div></body>",
        "html, body, div, a, span { margin:0 } \
         body { display:block } \
         #row { display:block; width:800px; height:62px; white-space:nowrap; font-size:12px } \
         #day { display:inline-block; vertical-align:top; height:16px; line-height:16px; \
                margin-top:1px; font-size:12px } \
         #tag { display:inline-block; vertical-align:top; width:16px; height:16px; \
                line-height:16px; margin-right:4px; font-size:12px } \
         #text { display:inline-block; vertical-align:top; margin-left:4px }",
        800.0,
    );
    let day = fragment(&layout, find(&output.dom, "#day")).rect;
    let tag = fragment(&layout, find(&output.dom, "#tag")).rect;
    let text = fragment(&layout, find(&output.dom, "#text")).rect;

    assert_eq!(day.size.height, 16.0, "{day:?}");
    assert_eq!(tag.origin.y, day.origin.y, "{tag:?} {day:?}");
    assert_eq!(text.origin.y, day.origin.y, "{text:?} {day:?}");
    assert_eq!(tag.size.height, 16.0);
    assert_eq!(text.size.height, 16.0);
    assert_eq!(day.origin.y, 1.0, "the anchor keeps its top margin");

    let text_node = output.dom.children(find(&output.dom, "#text")).unwrap()[0];
    let text_fragment = fragment(&layout, text_node);
    let FragmentKind::Text(data) = &text_fragment.kind else {
        panic!("expected the text run inside #text");
    };
    // 12px text in a 16px line: (16 - 12) / 2 + 0.8 * 12 = 2 + 9.6.
    assert!(
        (data.baseline - (text.origin.y + 11.6)).abs() < 0.01,
        "baseline {} in {text:?}",
        data.baseline
    );
}

/// The movie card from the portal fixture: a 106px image item and a 56px
/// information block are both `display:inline-block; vertical-align:top`.
/// The tall image must start at its parent's top edge, not be pushed down by
/// its baseline (the positioned order badge inside it used to be mistaken for
/// the item's baseline).
#[test]
fn movie_card_top_aligned_inline_blocks_start_at_the_parent_top() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='more'>\
           <div id='img'><div id='order'>1</div></div>\
           <div id='info'><p id='title'>阿凡达3</p></div>\
         </div></body>",
        "html, body, div, p { display:block; margin:0 } \
         #more { width:240px; font-size:12px; line-height:16px } \
         #img { display:inline-block; vertical-align:top; width:80px; height:106px; margin-right:12px } \
         #order { position:absolute; left:0; top:0; width:20px; height:20px } \
         #info { display:inline-block; vertical-align:top; width:138px; min-height:56px; margin:5px 0 } \
         #title { height:14px; line-height:14px; font-size:14px }",
        800.0,
    );
    let more = fragment(&layout, find(&output.dom, "#more")).rect;
    let image = fragment(&layout, find(&output.dom, "#img")).rect;
    let info = fragment(&layout, find(&output.dom, "#info")).rect;

    assert_eq!(image.origin.y, more.origin.y, "{image:?} {more:?}");
    assert_eq!(image.size.height, 106.0);
    assert_eq!(info.origin.y, more.origin.y + 5.0, "{info:?} {more:?}");
    assert_eq!(info.size.height, 56.0);
    assert_eq!(more.size.height, 106.0);
}

/// A 10px inline-block with a 16px font in a 20px line. The parent's content
/// area is 12.8px above and 3.2px below its baseline (the reference face is
/// 0.8em/0.2em), and its x-height is 8px, so each keyword has a closed-form
/// top offset. `baseline` puts the item's own 9.8px baseline on the line's
/// baseline.
#[test]
fn vertical_align_keywords_position_a_short_inline_block() {
    let cases = [
        ("baseline", 2.0),
        ("top", 0.0),
        ("bottom", 10.0),
        ("middle", 13.8),
        ("text-top", 2.0),
        ("text-bottom", 8.0),
    ];
    for (value, expected) in cases {
        let css = format!(
            "html, body, div, span {{ display:block; margin:0 }} \
             #row {{ width:400px; font-size:16px; line-height:20px }} \
             #item {{ display:inline-block; width:40px; height:10px; font-size:16px; \
                     line-height:16px; vertical-align:{value} }}"
        );
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='row'>base<span id='item'>x</span></div></body>",
            &css,
            400.0,
        );
        let row = fragment(&layout, find(&output.dom, "#row")).rect;
        let item = fragment(&layout, find(&output.dom, "#item")).rect;
        for fragment in layout.fragments.iter() {
            if let FragmentKind::Text(data) = &fragment.kind {
                eprintln!(
                    "{value}: text {:?} rect {:?} baseline {}",
                    data.text, fragment.rect, data.baseline
                );
            }
        }
        assert!(
            (item.origin.y - row.origin.y - expected).abs() < 0.01,
            "{value}: item at {} in row {row:?}",
            item.origin.y
        );
    }
}

/// §10.8.1's `<length>` form raises the item by that distance. The percentage
/// form resolves against the element's own `line-height`, which is 20px here,
/// so 25% is the same 5px shift.
#[test]
fn vertical_align_length_and_percentage_raise_the_baseline() {
    for (value, shift) in [("5px", 5.0), ("25%", 5.0), ("-3px", -3.0)] {
        let css = format!(
            "html, body, div {{ display:block; margin:0 }} \
             #row {{ width:400px; font-size:16px; line-height:20px }} \
             #raised {{ vertical-align:{value} }}"
        );
        let (_, _, layout) = pipeline(
            "<!doctype html><body><div id='row'>base<span id='raised'>up</span></div></body>",
            &css,
            400.0,
        );
        let baseline = |needle: &str| {
            layout
                .fragments
                .iter()
                .find_map(|fragment| match &fragment.kind {
                    FragmentKind::Text(text) if text.text.contains(needle) => Some(text.baseline),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no text fragment for {needle:?}"))
        };
        assert!(
            ((baseline("base") - baseline("up")) - shift).abs() < 0.01,
            "{value}: base {} raised {}",
            baseline("base"),
            baseline("up")
        );
    }
}

/// `top` and `bottom` are pinned to the line item, not to the parent's
/// baseline, so a item taller than the strut grows the line and then follows
/// the grown edge. A 40px item in a 20px line starts at the line top for
/// `top`/`bottom`; `text-top` starts 2px lower (the strut's half-leading) and
/// makes the line 42px tall by reaching 27.2px below the baseline.
#[test]
fn vertical_align_grows_the_line_box_to_contain_aligned_boxes() {
    let cases = [
        ("top", 0.0, 40.0),
        ("bottom", 0.0, 40.0),
        ("middle", 0.0, 40.0),
        ("text-top", 2.0, 42.0),
        ("text-bottom", 0.0, 42.0),
    ];
    for (value, expected_top, expected_height) in cases {
        let css = format!(
            "html, body, div, span {{ display:block; margin:0 }} \
             #row {{ width:400px; font-size:16px; line-height:20px }} \
             #item {{ display:inline-block; width:40px; height:40px; font-size:16px; \
                     line-height:20px; vertical-align:{value} }}"
        );
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='row'>base<span id='item'>x</span></div></body>",
            &css,
            400.0,
        );
        let row = fragment(&layout, find(&output.dom, "#row")).rect;
        let item = fragment(&layout, find(&output.dom, "#item")).rect;
        assert!(
            (item.origin.y - row.origin.y - expected_top).abs() < 0.01,
            "{value}: item {item:?} in row {row:?}"
        );
        assert!(
            (row.size.height - expected_height).abs() < 0.01,
            "{value}: row {row:?}"
        );
    }
}

/// A text run inside an inline element follows the same alignment: the line
/// grows upward for a raised span and the baseline distance is exactly the
/// requested shift.
#[test]
fn vertical_align_on_an_inline_span_moves_its_text_baseline() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><div id='row'>base<span id='raised'>up</span>tail</div></body>",
        "html, body, div { display:block; margin:0 } \
         body { display:block } \
         #row { width:400px; font-size:16px; line-height:20px } \
         #raised { vertical-align:middle }",
        400.0,
    );
    let baseline = |needle: &str| {
        layout
            .fragments
            .iter()
            .find_map(|fragment| match &fragment.kind {
                FragmentKind::Text(text) if text.text.contains(needle) => Some(text.baseline),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text fragment for {needle:?}"))
    };
    // 16px font, 20px line: strut ascent 14.8, x-height 8. `middle` aligns
    // the 20px text item's midpoint with baseline + 4, so its top is 6px below
    // the parent baseline, and its own 14.8px baseline lands 8.8px below it.
    assert!(((baseline("base") - baseline("up")) + 8.8).abs() < 0.01);
}

/// The default atomic alignment still puts an inline-block's own text
/// baseline on the line's baseline; `vertical-align: top` on the same item is
/// what changes where its top edge goes.
#[test]
fn default_baseline_still_aligns_an_inline_blocks_text_baseline() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='row'>text<span id='item'>item</span></div></body>",
        "html, body, div { display:block; margin:0 } \
         body { display:block } \
         #row { width:400px; font-size:16px; line-height:20px } \
         #item { display:inline-block; width:40px; height:26px; line-height:20px; font-size:16px }",
        400.0,
    );
    let row = fragment(&layout, find(&output.dom, "#row")).rect;
    let item = fragment(&layout, find(&output.dom, "#item")).rect;
    let baseline = |needle: &str| {
        layout
            .fragments
            .iter()
            .find_map(|fragment| match &fragment.kind {
                FragmentKind::Text(text) if text.text.contains(needle) => Some(text.baseline),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text fragment for {needle:?}"))
    };
    assert!((baseline("text") - baseline("item")).abs() < 0.01);
    assert_eq!(item.origin.y, row.origin.y);
    assert_eq!(row.size.height, 26.0);
    assert_eq!(
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#item")))
            .map(|fragment| fragment.rect),
        Some(PhysicalRect::new(32.0, 0.0, 40.0, 26.0))
    );
}
