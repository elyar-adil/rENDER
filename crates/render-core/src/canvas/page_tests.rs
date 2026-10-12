//! A canvas drawn by a page's script reaches the page's raster: the bitmap is
//! published into the image store and painted at the element's box.

use std::time::Duration;

use url::Url;

use crate::page::Page;
use crate::script::ScriptDiscoveryLimits;

#[test]
fn a_canvas_drawn_by_script_is_painted_into_the_page_raster() {
    let html = "<!doctype html><body style='margin:0'>\
        <canvas id=c width=4 height=4 style='display:block'></canvas>\
        <script>var ctx = document.getElementById('c').getContext('2d');\
        ctx.fillStyle = 'rgb(255, 0, 0)'; ctx.fillRect(0, 0, 4, 4);</script>";
    let mut page = Page::new(html);
    let base = Url::parse("https://example.test/page").expect("base URL");
    let queue = page.queue_document_scripts(&base, ScriptDiscoveryLimits::default());
    assert!(queue.errors.is_empty(), "the script is discovered");
    page.pump_until_idle_reference(Duration::ZERO)
        .expect("the page pumps");
    let surface = &page.snapshot().raster.surface;
    let inside = surface.pixel(1, 1).expect("the pixel is on the surface");
    assert_eq!((inside.red, inside.green, inside.blue), (255, 0, 0));
}
