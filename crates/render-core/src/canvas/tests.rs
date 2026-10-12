//! End-to-end checks of the 2D context: each test runs a script against a real
//! DOM and reads pixels back through `getImageData`, the way a conformance
//! test does.

#![allow(
    // Geometry and pixel code converts between float coordinates and pixel
    // indices. Every conversion is preceded by a clamp or a floor, and the
    // bounds are the canvas or path extent, so truncation is the intent.
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    // Exact comparisons are intended: a degenerate segment is exactly zero
    // length, and an edge is exactly horizontal.
    clippy::float_cmp,
    // The specification's own names (x1, cp2y, ...) are kept.
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::collapsible_if,
    clippy::single_match_else
)]

use render_js::{JsRuntime, JsValue};

use super::CanvasHandle;

/// Runs `script` on a page with one 10 by 10 `<canvas id="c">` and returns the
/// string it evaluates to.
fn run(script: &str) -> String {
    let output = render_html::parse_document(
        "<!doctype html><body><canvas id=c width=10 height=10></canvas>",
    );
    let mut dom = output.dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime.install_canvas_host(Box::new(CanvasHandle::default()));
    let outcome = runtime
        .execute(&mut dom, script)
        .unwrap_or_else(|error| panic!("the script failed: {error:?}"));
    match outcome.value {
        JsValue::String(text) => text.to_lossy(),
        other => panic!("the script did not return a string: {other:?}"),
    }
}

const SETUP: &str = "var c = document.getElementById('c'); var ctx = c.getContext('2d');";

/// The RGBA of pixel `(x, y)` as the script reads it.
fn pixel(x: u32, y: u32) -> String {
    format!(
        "(function(){{var d = ctx.getImageData({x}, {y}, 1, 1).data; return [d[0], d[1], d[2], d[3]].join(',');}})()"
    )
}

#[test]
fn an_unsupported_context_kind_is_null_and_the_2d_context_is_one_object() {
    assert_eq!(
        run("var c = document.getElementById('c'); String(c.getContext('webgl'))"),
        "null"
    );
    assert_eq!(
        run(
            "var c = document.getElementById('c'); String(c.getContext('2d') === c.getContext('2d'))"
        ),
        "true"
    );
}

#[test]
fn a_fresh_canvas_is_transparent_black() {
    assert_eq!(run(&format!("{SETUP} {}", pixel(3, 3))), "0,0,0,0");
}

#[test]
fn fill_rect_with_the_default_fill_style_paints_opaque_black_inside_only() {
    let script = format!(
        "{SETUP} ctx.fillRect(2, 2, 4, 4); [{}, {}, {}].join('|')",
        pixel(2, 2),
        pixel(5, 5),
        pixel(6, 6)
    );
    assert_eq!(run(&script), "0,0,0,255|0,0,0,255|0,0,0,0");
}

#[test]
fn fill_style_colours_and_serialization_round_trip() {
    let script = format!(
        "{SETUP} ctx.fillStyle = 'rgba(0, 255, 0, 0.5)'; var s = ctx.fillStyle; \
         ctx.fillRect(0, 0, 10, 10); s + ' ' + {}",
        pixel(4, 4)
    );
    assert_eq!(run(&script), "rgba(0, 255, 0, 0.5) 0,255,0,128");
    assert_eq!(
        run(&format!(
            "{SETUP} ctx.fillStyle = 'not a colour'; ctx.fillStyle"
        )),
        "#000000"
    );
}

#[test]
fn global_alpha_scales_the_source_alpha() {
    let script = format!(
        "{SETUP} ctx.globalAlpha = 0.5; ctx.fillStyle = 'rgb(0, 0, 255)'; ctx.fillRect(0, 0, 10, 10); {}",
        pixel(1, 1)
    );
    assert_eq!(run(&script), "0,0,255,128");
}

#[test]
fn clear_rect_removes_what_was_painted() {
    let script = format!(
        "{SETUP} ctx.fillRect(0, 0, 10, 10); ctx.clearRect(2, 2, 3, 3); [{}, {}].join('|')",
        pixel(3, 3),
        pixel(0, 0)
    );
    assert_eq!(run(&script), "0,0,0,0|0,0,0,255");
}

#[test]
fn save_and_restore_bring_back_the_transform_and_the_style() {
    let script = format!(
        "{SETUP} ctx.save(); ctx.translate(5, 0); ctx.fillStyle = 'red'; ctx.fillRect(0, 0, 2, 2); \
         ctx.restore(); ctx.fillStyle = 'blue'; ctx.fillRect(0, 0, 1, 1); \
         [{}, {}, {}].join('|')",
        pixel(6, 0),
        pixel(0, 0),
        pixel(1, 0)
    );
    assert_eq!(run(&script), "255,0,0,255|0,0,255,255|0,0,0,0");
}

#[test]
fn a_filled_circle_covers_its_centre_and_not_its_corner() {
    let script = format!(
        "{SETUP} ctx.beginPath(); ctx.arc(5, 5, 4, 0, Math.PI * 2); ctx.fill(); [{}, {}].join('|')",
        pixel(5, 5),
        pixel(0, 0)
    );
    assert_eq!(run(&script), "0,0,0,255|0,0,0,0");
}

#[test]
fn stroke_rect_paints_a_border_and_leaves_the_middle_open() {
    let script = format!(
        "{SETUP} ctx.lineWidth = 2; ctx.strokeRect(2, 2, 6, 6); [{}, {}].join('|')",
        pixel(2, 4),
        pixel(5, 5)
    );
    assert_eq!(run(&script), "0,0,0,255|0,0,0,0");
}

#[test]
fn put_image_data_writes_straight_alpha_pixels_back_unchanged() {
    let script = format!(
        "{SETUP} var img = ctx.createImageData(2, 1); img.data[0] = 10; img.data[1] = 20; \
         img.data[2] = 30; img.data[3] = 255; ctx.putImageData(img, 4, 4); \
         var back = ctx.getImageData(4, 4, 1, 1).data; [back[0], back[1], back[2], back[3]].join(',')"
    );
    assert_eq!(run(&script), "10,20,30,255");
}

#[test]
fn a_negative_arc_radius_throws_index_size_error() {
    let script = format!(
        "{SETUP} var name = 'none'; try {{ ctx.arc(0, 0, -1, 0, 1); }} catch (e) {{ name = e.name; }} name"
    );
    assert_eq!(run(&script), "IndexSizeError");
}

#[test]
fn a_negative_dash_is_ignored_and_a_valid_one_is_read_back() {
    let script = format!(
        "{SETUP} ctx.setLineDash([1, 2]); ctx.setLineDash([-1, 2]); ctx.getLineDash().join(',')"
    );
    assert_eq!(run(&script), "1,2");
}
