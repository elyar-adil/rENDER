//! The bounds a script cannot push past: oversize images throw instead of
//! allocating, a pathological dash finishes, and offscreen canvases draw.

use super::tests::run;

#[test]
fn an_oversize_image_data_throws_a_range_error_before_allocating() {
    assert_eq!(
        run(
            "var c = document.getElementById('c'); var ctx = c.getContext('2d'); \
             var name = 'none'; try { ctx.createImageData(100000, 100000); } \
             catch (e) { name = e.name; } name"
        ),
        "RangeError"
    );
    assert_eq!(
        run("var name = 'none'; try { new ImageData(100000, 100000); } \
             catch (e) { name = e.name; } name"),
        "RangeError"
    );
}

#[test]
fn an_oversize_get_image_data_throws_a_range_error() {
    assert_eq!(
        run(
            "var ctx = document.getElementById('c').getContext('2d'); var name = 'none'; \
             try { ctx.getImageData(0, 0, 100000, 100000); } catch (e) { name = e.name; } name"
        ),
        "RangeError"
    );
}

#[test]
fn a_zero_sized_image_data_is_an_index_size_error() {
    assert_eq!(
        run("var name = 'none'; try { new ImageData(0, 1); } catch (e) { name = e.name; } name"),
        "IndexSizeError"
    );
}

#[test]
fn a_dash_far_shorter_than_the_path_finishes_and_still_strokes() {
    assert_eq!(
        run("var ctx = document.getElementById('c').getContext('2d'); \
             ctx.setLineDash([0.0000001, 0.0000001]); ctx.lineWidth = 2; \
             ctx.beginPath(); ctx.moveTo(0, 5); ctx.lineTo(10, 5); ctx.stroke(); \
             var d = ctx.getImageData(5, 5, 1, 1).data; String(d[3] > 0)"),
        "true"
    );
}

#[test]
fn an_offscreen_canvas_has_its_own_2d_context_and_pixels() {
    assert_eq!(
        run(
            "var o = new OffscreenCanvas(4, 4); var ctx = o.getContext('2d'); \
             ctx.fillStyle = 'rgb(0, 0, 255)'; ctx.fillRect(0, 0, 4, 4); \
             var d = ctx.getImageData(2, 2, 1, 1).data; \
             [o.width, o.height, d[0], d[1], d[2], d[3], o.getContext('2d') === ctx].join(',')"
        ),
        "4,4,0,0,255,255,true"
    );
}

#[test]
fn setting_an_offscreen_size_resets_its_bitmap() {
    assert_eq!(
        run(
            "var o = new OffscreenCanvas(4, 4); var ctx = o.getContext('2d'); \
             ctx.fillRect(0, 0, 4, 4); o.width = 2; \
             var d = ctx.getImageData(0, 0, 1, 1).data; [o.width, d[3]].join(',')"
        ),
        "2,0"
    );
}
