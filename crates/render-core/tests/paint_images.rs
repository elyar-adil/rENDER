use render_core::document::{Document, DocumentBackends, DocumentRenderOptions};
use render_core::image::{DecodedImage, ImageLimits, ImageResources, discover_images_with_styles};
use render_core::layout::SimpleTextMeasurer;
use render_core::paint::{Color, NoGlyphMasks, ReferenceTextShaper};
use url::Url;

fn target_id(document: &Document, id: &str) -> render_core::dom::NodeId {
    let mut pending = vec![document.dom().document()];
    while let Some(node) = pending.pop() {
        if document.dom().attribute(node, "id").ok().flatten() == Some(id) {
            return node;
        }
        pending.extend(
            document
                .dom()
                .children(node)
                .unwrap_or_default()
                .iter()
                .rev(),
        );
    }
    panic!("missing test element #{id}");
}

#[test]
fn background_image_uses_border_box_as_the_default_painting_area() {
    let document = Document::parse(
        "<!doctype html><style>\
         html, body { display:block; margin:0 }\
         #box { display:block; width:20px; height:20px;\
                border:4px solid transparent;\
                background-image:url(bg.png);\
                background-repeat:no-repeat;\
                background-position:center }\
         </style><div id=box></div>",
    );
    let url = Url::parse("https://example.test/page.html").unwrap();
    let initial = document.render_reference(DocumentRenderOptions::default());
    let discovery = discover_images_with_styles(
        document.dom(),
        &initial.styles,
        &url,
        ImageLimits::default(),
    );
    let key = discovery
        .resources
        .iter()
        .find(|resource| resource.key.source_snapshot == "url(bg.png)")
        .expect("background image should be discovered")
        .key
        .clone();
    let image = DecodedImage::from_pixels(40, 40, vec![Color::rgb(255, 0, 0); 40 * 40]).unwrap();
    let mut images = ImageResources::default();
    images.insert(key, image, ImageLimits::default()).unwrap();

    let render = document.render_with_images(
        DocumentRenderOptions::default(),
        DocumentBackends {
            text_measurer: &SimpleTextMeasurer,
            text_shaper: &ReferenceTextShaper,
            glyph_masks: &NoGlyphMasks,
        },
        &images,
    );
    let box_node = target_id(&document, "box");
    let image_item = render
        .display
        .list
        .items()
        .iter()
        .find(|item| {
            item.source == Some(box_node)
                && matches!(item.command, render_core::paint::DisplayCommand::Image(_))
        })
        .expect("background image should be painted");
    #[allow(
        clippy::float_cmp,
        reason = "the geometry is derived from exact integral pixel values"
    )]
    {
        assert_eq!(image_item.bounds.origin.x, -6.0);
        assert_eq!(image_item.bounds.origin.y, -6.0);
    }

    // The box's 4px transparent border is part of the default border-box
    // painting area, so the centered 40px image remains visible at (1, 1).
    assert_eq!(
        render.raster.surface.pixel(1, 1),
        Some(Color::rgb(255, 0, 0))
    );
}
