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

#[test]
fn video_element_paints_its_presented_frame() {
    let document = Document::parse(
        "<!doctype html><style>html, body { display:block; margin:0 }</style>\
         <video id=player width=64 height=48 src='movie.mp4'></video>",
    );
    let node = target_id(&document, "player");
    let media_url = Url::parse("https://example.test/movie.mp4").unwrap();
    let mut images = ImageResources::default();

    // Before playback the box paints nothing beyond the empty element.
    let initial = document.render(
        DocumentRenderOptions::default(),
        DocumentBackends {
            text_measurer: &SimpleTextMeasurer,
            text_shaper: &ReferenceTextShaper,
            glyph_masks: &NoGlyphMasks,
        },
    );
    assert!(!initial.display.list.items().iter().any(|item| {
        item.source == Some(node)
            && matches!(item.command, render_core::paint::DisplayCommand::Image(_))
    }));

    // The presentation clock publishes one 64x48 frame for the element.
    let mut frame_bytes = Vec::with_capacity(64 * 48 * 4);
    for _ in 0..64 * 48 {
        frame_bytes.extend_from_slice(&[13, 200, 60, 255]);
    }
    let frame = DecodedImage::from_rgba8(64, 48, &frame_bytes).unwrap();
    images
        .set_video_frame(node, &media_url, frame, ImageLimits::default())
        .unwrap();

    let render = document.render_with_images(
        DocumentRenderOptions::default(),
        DocumentBackends {
            text_measurer: &SimpleTextMeasurer,
            text_shaper: &ReferenceTextShaper,
            glyph_masks: &NoGlyphMasks,
        },
        &images,
    );
    let image_item = render
        .display
        .list
        .items()
        .iter()
        .find(|item| {
            item.source == Some(node)
                && matches!(item.command, render_core::paint::DisplayCommand::Image(_))
        })
        .expect("the presented frame paints as an image command");
    #[allow(
        clippy::float_cmp,
        reason = "the geometry is derived from exact integral pixel values"
    )]
    {
        assert_eq!(image_item.bounds.size.width, 64.0);
        assert_eq!(image_item.bounds.size.height, 48.0);
    }
    // Layout used the frame's intrinsic size and the raster sampled it.
    assert_eq!(
        render.raster.surface.pixel(10, 10),
        Some(Color::rgb(13, 200, 60))
    );

    // Dropping the frame returns the element to its empty presentation.
    images.remove_video_frame(node);
    let cleared = document.render_with_images(
        DocumentRenderOptions::default(),
        DocumentBackends {
            text_measurer: &SimpleTextMeasurer,
            text_shaper: &ReferenceTextShaper,
            glyph_masks: &NoGlyphMasks,
        },
        &images,
    );
    assert!(!cleared.display.list.items().iter().any(|item| {
        item.source == Some(node)
            && matches!(item.command, render_core::paint::DisplayCommand::Image(_))
    }));
}

#[test]
fn absolutely_positioned_img_fills_its_padding_top_placeholder() {
    // The real-world card pattern: an aspect-ratio placeholder box and an
    // absolutely positioned cover image stretched over its padding box.
    let document = Document::parse(
        "<!doctype html><style>\
         html, body { display:block; margin:0 }\
         .card { position:relative; width:200px; padding-top:56.25% }\
         .cover { position:absolute; inset:0; width:100%; height:100%; object-fit:cover }\
         </style>\
         <div class=card><img id=cover class=cover src='cover.png'></div>",
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
        .first()
        .expect("cover image should be discovered")
        .key
        .clone();
    let image = DecodedImage::from_pixels(64, 64, vec![Color::rgb(255, 0, 0); 64 * 64]).unwrap();
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
    let cover = target_id(&document, "cover");
    let item = render
        .display
        .list
        .items()
        .iter()
        .find(|item| {
            item.source == Some(cover)
                && matches!(item.command, render_core::paint::DisplayCommand::Image(_))
        })
        .expect("the absolutely positioned cover must paint");
    #[allow(
        clippy::float_cmp,
        reason = "the geometry is derived from exact integral pixel values"
    )]
    {
        // Percentage padding-top resolves against the containing block width
        // (1280), so the placeholder is 200 wide and 720 tall; the cover
        // fills its padding box edge to edge. object-fit: cover on a square
        // source scales the paint up to 720x720, centered on the box.
        assert_eq!(item.bounds.origin.x, -260.0);
        assert_eq!(item.bounds.origin.y, 0.0);
        assert_eq!(item.bounds.size.width, 720.0);
        assert_eq!(item.bounds.size.height, 720.0);
    }
    // The cover fills the placeholder: the middle of the box samples red.
    assert_eq!(
        render.raster.surface.pixel(100, 56),
        Some(Color::rgb(255, 0, 0))
    );
    // object-fit must crop to the box: pixels right of the 200px-wide
    // placeholder stay unpainted even though the cover-scaled source is 720
    // wide.
    assert_ne!(
        render.raster.surface.pixel(300, 100),
        Some(Color::rgb(255, 0, 0))
    );
}
