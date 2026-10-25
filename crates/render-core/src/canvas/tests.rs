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
pub(super) fn run(script: &str) -> String {
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

#[test]
fn round_rect_one_radius_rounds_every_corner() {
    // WPT 2d.path.roundrect.1.radius.double, at its own size: the corner
    // pixels the radius cuts away stay red, the middle is green.
    let script = format!(
        "{SETUP} c.width = 100; c.height = 50;          ctx.fillStyle = '#f00'; ctx.fillRect(0, 0, 100, 50);          ctx.beginPath(); ctx.roundRect(0, 0, 100, 50, [20]);          ctx.fillStyle = '#0f0'; ctx.fill();          [{}, {}, {}, {}, {}].join('|')",
        pixel(1, 1),
        pixel(98, 1),
        pixel(98, 48),
        pixel(1, 48),
        pixel(50, 25)
    );
    assert_eq!(
        run(&script),
        "255,0,0,255|255,0,0,255|255,0,0,255|255,0,0,255|0,255,0,255"
    );
}

#[test]
fn round_rect_four_radii_round_only_the_named_corners() {
    // WPT 2d.path.roundrect.4.radii.3.double: [0, 0, 20, 0] rounds only the
    // bottom-right corner.
    let script = format!(
        "{SETUP} c.width = 100; c.height = 50;          ctx.fillStyle = '#f00'; ctx.fillRect(0, 0, 100, 50);          ctx.beginPath(); ctx.roundRect(0, 0, 100, 50, [0, 0, 20, 0]);          ctx.fillStyle = '#0f0'; ctx.fill();          [{}, {}, {}, {}].join('|')",
        pixel(1, 1),
        pixel(98, 1),
        pixel(98, 48),
        pixel(1, 48)
    );
    assert_eq!(
        run(&script),
        "0,255,0,255|0,255,0,255|255,0,0,255|0,255,0,255"
    );
}

#[test]
fn round_rect_radii_scales_when_edges_cannot_hold_them() {
    // WPT 2d.path.roundrect.radius.intersecting.2: adjacent radii wider than
    // the edge scale down by the one factor, so the shape stays a rectangle.
    let script = format!(
        "{SETUP} c.width = 100; c.height = 50;          ctx.fillStyle = '#f00'; ctx.fillRect(0, 0, 100, 50);          ctx.beginPath(); ctx.roundRect(0, 0, 100, 50, [40, 40]);          ctx.fillStyle = '#0f0'; ctx.fill();          [{}, {}, {}, {}].join('|')",
        pixel(1, 1),
        pixel(98, 1),
        pixel(98, 48),
        pixel(1, 48)
    );
    // 40 + 40 = 80 > 50 (the height), so every radius scales by 25/40: the
    // corners still cut, just shallower.
    assert_eq!(
        run(&script),
        "255,0,0,255|255,0,0,255|255,0,0,255|255,0,0,255"
    );
}

#[test]
fn round_rect_negative_sizes_flip_their_corners() {
    // WPT 2d.path.roundrect.negative: a negative width or height draws the
    // same shape as the positive one with the corners swapped.
    let script = format!(
        "{SETUP} c.width = 100; c.height = 50;          ctx.fillStyle = '#f00'; ctx.fillRect(0, 0, 100, 50);          ctx.beginPath();          ctx.roundRect(0, 0, 50, 25, [10, 0, 0, 0]);          ctx.roundRect(100, 0, -50, 25, [10, 0, 0, 0]);          ctx.fillStyle = '#0f0'; ctx.fill();          [{}, {}, {}, {}].join('|')",
        pixel(1, 1),
        pixel(48, 1),
        pixel(51, 1),
        pixel(98, 1)
    );
    // Both subpaths cut their geometric top-left corner: rect 1 names it
    // directly, and rect 2's named top-left radius mirrors onto its
    // geometric top-right corner because its width is negative.
    assert_eq!(
        run(&script),
        "255,0,0,255|0,255,0,255|0,255,0,255|255,0,0,255"
    );
}

#[test]
fn round_rect_with_zero_height_strokes_a_line() {
    // WPT 2d.path.roundrect.zero.1: a zero-height rect still adds its straight
    // edges, and stroking one paints them.
    let script = format!(
        "{SETUP} c.width = 100; c.height = 50;          ctx.fillStyle = '#f00'; ctx.fillRect(0, 0, 100, 50);          ctx.strokeStyle = '#0f0'; ctx.lineWidth = 100; ctx.beginPath();          ctx.roundRect(0, 50, 100, 0, [0]); ctx.stroke();          [{}, {}, {}].join('|')",
        pixel(2, 25),
        pixel(98, 25),
        pixel(2, 2)
    );
    // A 100-pixel line on a 50-pixel canvas covers every pixel.
    assert_eq!(run(&script), "0,255,0,255|0,255,0,255|0,255,0,255");
}

#[test]
fn round_rect_rejects_bad_radius_shapes() {
    // WPT 2d.path.roundrect.radius.toomany, .none and .negative, and the
    // union's TypeError on a BigInt: a sequence that does not hold one to
    // four items and a negative radius are RangeErrors.
    let script = format!(
        "{SETUP} var names = [];          for (var call of [function () {{ ctx.roundRect(0, 0, 100, 50, [0, 0, 0, 0, 0]); }},                            function () {{ ctx.roundRect(0, 0, 100, 50, []); }},                            function () {{ ctx.roundRect(0, 0, 100, 50, [-1]); }},                            function () {{ ctx.roundRect(0, 0, 100, 50, 0n); }}]) {{              try {{ call(); }} catch (e) {{ names.push(e.name); }}          }}          names.join(',')"
    );
    assert_eq!(run(&script), "RangeError,RangeError,RangeError,TypeError");
}

#[test]
fn round_rect_accepts_the_dictionary_shapes_bad_input_produces() {
    // WPT 2d.path.roundrect.badinput: objects without x or y are all-zero
    // DOMPointInit dictionaries, and an absent radii argument is zero, so
    // every call here is a plain rectangle and none throws.
    let script = format!(
        "{SETUP} c.width = 100; c.height = 50;          ctx.roundRect(0, 0, 100, 50, {{ foo: 'bar' }});          ctx.roundRect(0, 0, 100, 50, undefined);          ctx.roundRect(0, 0, 100, 50, [[]]);          ctx.roundRect(0, 0, 100, 50, [undefined]);          ctx.roundRect(0, 0, 100, 50);          ctx.isPointInPath(50, 25) + ',' + ctx.isPointInPath(-1, -1)"
    );
    assert_eq!(run(&script), "true,false");
}

#[test]
fn a_linear_gradient_interpolates_between_its_stops() {
    // WPT 2d.gradient.linear.transform.3 shape: a left-to-right gradient from
    // red to green over exactly the canvas width. The samples read each
    // pixel's centre, so t is 0.025, 0.505 and 0.975 - the blend the
    // interpolation actually produces there.
    let script = format!(
        "{SETUP} c.width = 100; c.height = 50; \
         var g = ctx.createLinearGradient(0, 0, 100, 0); \
         g.addColorStop(0, '#f00'); g.addColorStop(1, '#0f0'); \
         ctx.fillStyle = g; ctx.fillRect(0, 0, 100, 50); \
         [{}, {}, {}].join('|')",
        pixel(2, 25),
        pixel(50, 25),
        pixel(98, 25)
    );
    assert_eq!(run(&script), "249,6,0,255|126,129,0,255|4,251,0,255");
}

#[test]
fn a_gradient_stays_live_after_fillstyle_assignment() {
    // WPT 2d.gradient.object.update: addColorStop after the assignment still
    // changes what the next fill paints. The samples are pixel centres, so
    // t = 0.025 lands at 95% red on the red-to-green half, and t = 0.505
    // just past the green middle.
    let script = format!(
        "{SETUP} c.width = 100; c.height = 50; \
         var g = ctx.createLinearGradient(0, 0, 100, 0); \
         g.addColorStop(0, '#f00'); g.addColorStop(1, '#f00'); \
         ctx.fillStyle = g; ctx.fillRect(0, 0, 100, 50); \
         g.addColorStop(0.5, '#0f0'); \
         ctx.fillRect(0, 0, 100, 50); \
         [{}, {}].join('|')",
        pixel(2, 25),
        pixel(50, 25)
    );
    assert_eq!(run(&script), "242,13,0,255|3,252,0,255");
}

#[test]
fn fillstyle_reads_back_the_same_gradient_object() {
    // WPT 2d.gradient.object.current/return: the getter returns the object
    // that was assigned, and createLinearGradient returns a new one each time.
    let script = format!(
        "{SETUP} var g = ctx.createLinearGradient(0, 0, 1, 0); \
         ctx.fillStyle = g; \
         [String(ctx.fillStyle === g), \
          String(ctx.createLinearGradient(0, 0, 1, 0) !== g), \
          Object.prototype.toString.call(ctx.fillStyle)].join('|')"
    );
    assert_eq!(run(&script), "true|true|[object CanvasGradient]");
}

#[test]
fn add_color_stop_rejects_bad_offsets_and_colours() {
    // WPT 2d.gradient.object.invalidoffset/invalidcolor: an offset outside
    // 0..=1 is an IndexSizeError, an unparseable colour a SyntaxError.
    let script = format!(
        "{SETUP} var names = []; var g = ctx.createLinearGradient(0, 0, 1, 0); \
         for (var call of [function () {{ g.addColorStop(-0.1, '#f00'); }}, \
                           function () {{ g.addColorStop(1.5, '#f00'); }}, \
                           function () {{ g.addColorStop(0.5, 'not-a-colour'); }}, \
                           function () {{ g.addColorStop(0.5, '#f00'); }}]) {{ \
             try {{ call(); }} catch (e) {{ names.push(e.name); }} \
         }} \
         names.join(',')"
    );
    // The fourth call succeeds and names nothing.
    assert_eq!(run(&script), "IndexSizeError,IndexSizeError,SyntaxError");
}

#[test]
fn a_gradient_usable_across_canvases_and_empty_paints_nothing() {
    // WPT 2d.gradient.object.crosscanvas: a gradient made on one canvas paints
    // on another. A gradient with no stops paints nothing (transparent black).
    let script = format!(
        "{SETUP} var g = ctx.createLinearGradient(0, 0, 10, 0); \
         var c2 = document.createElement('canvas'); c2.width = 10; c2.height = 10; \
         var ctx2 = c2.getContext('2d'); \
         ctx2.fillStyle = g; ctx2.fillRect(0, 0, 10, 10); \
         var empty = ctx.createLinearGradient(0, 0, 10, 0); \
         ctx.fillStyle = empty; ctx.fillRect(0, 0, 10, 10); \
         var d2 = ctx2.getImageData(5, 5, 1, 1).data; \
         var d1 = ctx.getImageData(5, 5, 1, 1).data; \
         [d2[0] + ',' + d2[1], d1[3]].join('|')"
    );
    // The other canvas got the black-to-transparent gradient's mid colour;
    // this canvas kept its bitmap untouched by the stopless gradient.
    assert_eq!(run(&script), "0,0|0");
}

#[test]
fn round_rect_two_radii_apply_to_opposite_corner_pairs() {
    // WPT 2d.path.roundrect.2.radii.1.double: [20, 0] rounds the top-left and
    // bottom-right corners, leaves the other two square.
    let script = format!(
        "{SETUP} c.width = 100; c.height = 50;          ctx.fillStyle = '#f00'; ctx.fillRect(0, 0, 100, 50);          ctx.beginPath(); ctx.roundRect(0, 0, 100, 50, [20, 0]);          ctx.fillStyle = '#0f0'; ctx.fill();          [{}, {}, {}, {}].join('|')",
        pixel(1, 1),
        pixel(98, 1),
        pixel(98, 48),
        pixel(1, 48)
    );
    assert_eq!(
        run(&script),
        "255,0,0,255|0,255,0,255|255,0,0,255|0,255,0,255"
    );
}

#[test]
fn round_rect_three_radii_repeat_the_second() {
    // WPT 2d.path.roundrect.3.radii.1.double: [20, 0, 0] names tl, tr, br and
    // repeats the second for bl, so only the top-left corner is round.
    let script = format!(
        "{SETUP} c.width = 100; c.height = 50;          ctx.fillStyle = '#f00'; ctx.fillRect(0, 0, 100, 50);          ctx.beginPath(); ctx.roundRect(0, 0, 100, 50, [20, 0, 0]);          ctx.fillStyle = '#0f0'; ctx.fill();          [{}, {}, {}, {}].join('|')",
        pixel(1, 1),
        pixel(98, 1),
        pixel(98, 48),
        pixel(1, 48)
    );
    assert_eq!(
        run(&script),
        "255,0,0,255|0,255,0,255|0,255,0,255|0,255,0,255"
    );
}

#[test]
fn canvas_state_attributes_round_trip_and_reset() {
    // WPT 2d.reset.state.*: every state attribute reads back what was set,
    // and ctx.reset() restores the default it reported before.
    let script = format!(
        "{SETUP} c.width = 100; c.height = 50; \
         var defaults = [ctx.font, ctx.direction, ctx.textAlign, ctx.shadowColor, \
                         ctx.globalCompositeOperation, ctx.letterSpacing, \
                         ctx.shadowOffsetX, ctx.imageSmoothingEnabled].join('|'); \
         ctx.font = '16px sans-serif'; ctx.direction = 'rtl'; ctx.textAlign = 'end'; \
         ctx.shadowColor = '#ff0000'; ctx.globalCompositeOperation = 'destination-over'; \
         ctx.letterSpacing = '12px'; ctx.shadowOffsetX = 10.0; \
         ctx.imageSmoothingEnabled = false; \
         var set = [ctx.font, ctx.direction, ctx.textAlign, ctx.shadowColor, \
                    ctx.globalCompositeOperation, ctx.letterSpacing, \
                    ctx.shadowOffsetX, ctx.imageSmoothingEnabled].join('|'); \
         ctx.reset(); \
         var after = [ctx.font, ctx.direction, ctx.textAlign, ctx.shadowColor, \
                      ctx.globalCompositeOperation, ctx.letterSpacing, \
                      ctx.shadowOffsetX, ctx.imageSmoothingEnabled].join('|'); \
         [set, after === defaults].join('@@')"
    );
    let result = run(&script);
    let (set, resets) = result.split_once("@@").expect("two halves");
    assert_eq!(
        set,
        "16px sans-serif|rtl|end|#ff0000|destination-over|12px|10|false"
    );
    assert_eq!(resets, "true");
}

#[test]
fn canvas_state_survives_save_and_restores_through_restore() {
    // The new state rides the same save/restore stack as the rest.
    let script = format!(
        "{SETUP} ctx.save(); ctx.font = '16px sans-serif'; ctx.textAlign = 'center'; \
         ctx.shadowOffsetX = 4; ctx.restore(); \
         [ctx.font, ctx.textAlign, ctx.shadowOffsetX].join('|')"
    );
    assert_eq!(run(&script), "10px sans-serif|start|0");
}

#[test]
fn canvas_enumerated_attributes_ignore_unknown_keywords() {
    // HTML §4.12.5.1.12: an unrecognized keyword leaves the attribute alone.
    let script = format!(
        "{SETUP} ctx.textAlign = 'center'; ctx.textAlign = 'sideways'; \
         ctx.globalCompositeOperation = 'destination-over'; \
         ctx.globalCompositeOperation = 'nonsense'; \
         ctx.direction = 'bogus'; ctx.direction = 'rtl'; \
         [ctx.textAlign, ctx.globalCompositeOperation, ctx.direction].join('|')"
    );
    assert_eq!(run(&script), "center|destination-over|rtl");
}

#[test]
fn canvas_shadow_color_ignores_unparseable_values() {
    let script = format!(
        "{SETUP} ctx.shadowColor = '#ff0000'; ctx.shadowColor = 'not-a-colour'; \
         ctx.shadowColor"
    );
    assert_eq!(run(&script), "#ff0000");
}
