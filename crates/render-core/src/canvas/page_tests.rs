//! A canvas drawn by a page's script reaches the page's raster: the bitmap is
//! published into the image store and painted at the element's box.

use std::time::Duration;

use url::Url;

use crate::page::Page;
use crate::script::ScriptDiscoveryLimits;

/// An embedder that renders a document clone on another thread has its own
/// network image store; the page's canvas store must be mergeable into it.
#[test]
fn canvas_pixels_can_be_moved_into_an_embedders_image_store() {
    let html = "<!doctype html><body style='margin:0'>\
        <canvas id=c width=4 height=4 style='display:block'></canvas>\
        <script>var ctx = document.getElementById('c').getContext('2d');\
        ctx.fillStyle = 'rgb(0, 0, 255)'; ctx.fillRect(0, 0, 4, 4);</script>";
    let mut page = Page::new(html);
    let base = Url::parse("https://example.test/page").expect("base URL");
    let queue = page.queue_document_scripts(&base, ScriptDiscoveryLimits::default());
    assert!(queue.errors.is_empty(), "the script is discovered");
    // No reference render here: this is the native browser's path, where the
    // page pumps turns and a separate worker performs the render.
    page.pump_at_most_without_render(Duration::ZERO, 32)
        .expect("the page pumps");
    let mut pending = vec![page.document().dom().document()];
    let mut canvas = None;
    while let Some(node) = pending.pop() {
        if page.document().dom().attribute(node, "id").ok().flatten() == Some("c") {
            canvas = Some(node);
            break;
        }
        pending.extend(
            page.document()
                .dom()
                .children(node)
                .unwrap_or_default()
                .iter()
                .copied(),
        );
    }
    let canvas = canvas.expect("canvas node");
    let (changed, frames) = page.publish_render_resources();
    assert!(changed, "the drawn bitmap is a new frame resource");
    assert!(frames.get_for_node(canvas).is_some());
    let mut embedded = crate::image::ImageResources::default();
    embedded
        .overlay(&frames, crate::image::ImageLimits::default())
        .expect("the canvas bitmap fits the embedder store");
    let loaded = embedded
        .get_for_node(canvas)
        .expect("the embedder store holds the canvas bitmap");
    assert_eq!(loaded.image.pixels().len(), 16);
}

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
