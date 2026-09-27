//! Measure what a page's inline `<svg>` elements cost per render pass.
//!
//! `discover_inline_svgs` rasterises on every call.
//! `InlineSvgDiscovery::install` is idempotent - unchanged markup keeps its
//! resource id and is not re-inserted - but the rasterisation itself is not
//! skipped, so the question this answers is whether that is visible inside a
//! render loop.
//!
//! The real-site fixtures in `tests/fixtures/real_sites/` are stripped
//! server-rendered captures and carry at most one `<svg>` between them, so they
//! cannot answer it. This builds a synthetic page with a configurable icon
//! count instead, and reports the same numbers in absolute terms so the ratio
//! can be checked against a real page rather than believed.
//!
//! Usage: `cargo run --release -p render-core --example inline_svg_cost -- [icons] [rounds]`
//! Defaults: 500 icons, 20 rounds. Release only - the numbers are meaningless in
//! a debug build, where the polygon fill alone is orders of magnitude slower.

use std::time::Instant;

use render_core::document::{Document, DocumentRenderOptions};
use render_core::image::inline_svg::discover_inline_svgs;
use render_core::image::{ImageLimits, ImageResources};

/// One icon as a page would write it: a 24x24 element with a viewBox, a group
/// carrying a transform, and two paths - the shape of a real icon-font
/// replacement.
const ICON: &str = "<svg class=\"icon\" width=\"24\" height=\"24\" viewBox=\"0 0 24 24\">\
     <g transform=\"translate(2 2)\">\
     <path d=\"M2 2 L20 2 L20 18 L2 18 Z\" fill=\"#333\"/>\
     <path d=\"M6 6 C8 4 12 4 14 6 S20 10 18 12\" stroke=\"#666\" fill=\"none\"/>\
     </g></svg>";

fn synthetic_page(icons: usize) -> String {
    let body = ICON.repeat(icons);
    format!("<!doctype html><style>body{{margin:0}} .icon{{display:inline-block}}</style>{body}")
}

fn median(mut samples: Vec<std::time::Duration>) -> f64 {
    samples.sort_unstable();
    samples[samples.len() / 2].as_secs_f64()
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let icons: usize = arguments
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(500);
    let rounds: usize = arguments
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20);

    let document = Document::parse(&synthetic_page(icons));
    let limits = ImageLimits::default();
    let discovery = discover_inline_svgs(document.dom(), limits);
    println!("page: {icons} inline icons");
    println!(
        "discovery: {} rasters, {} diagnostics",
        discovery.resources.len(),
        discovery.diagnostics.len()
    );
    let markup_bytes: usize = discovery
        .resources
        .iter()
        .map(|raster| raster.source.len())
        .sum();
    let raster_pixels: u64 = discovery
        .resources
        .iter()
        .map(|raster| u64::from(raster.image.width()) * u64::from(raster.image.height()))
        .sum();
    println!(
        "markup: {markup_bytes} bytes serialised, {raster_pixels} device pixels rasterised \
         ({} per icon)",
        raster_pixels / u64::try_from(discovery.resources.len().max(1)).unwrap_or(1)
    );

    // A cold store: every round rasterises and every round inserts.
    let mut cold = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let mut images = ImageResources::default();
        let started = Instant::now();
        let pass = discover_inline_svgs(document.dom(), limits);
        let _ = pass.install(&mut images, limits);
        cold.push(started.elapsed());
    }

    // A warm store: install finds the unchanged markup and keeps the resource,
    // so this is the rasterisation cost alone with no allocation or store work.
    let mut images = ImageResources::default();
    let _ = discover_inline_svgs(document.dom(), limits).install(&mut images, limits);
    let mut warm = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        let pass = discover_inline_svgs(document.dom(), limits);
        let _ = pass.install(&mut images, limits);
        warm.push(started.elapsed());
    }

    // The whole headless render, for scale: this is what a frame actually costs.
    let mut renders = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        let _ = document.render_reference(DocumentRenderOptions::default());
        renders.push(started.elapsed());
    }

    let cold = median(cold) * 1_000.0;
    let warm = median(warm) * 1_000.0;
    let render = median(renders) * 1_000.0;
    println!("median of {rounds} rounds, milliseconds");
    println!("  discover + install, cold store : {cold:.3}");
    println!("  discover + install, warm store : {warm:.3}");
    println!("  full render_reference         : {render:.3}");
    println!(
        "  inline-SVG share of a frame   : {:.1}% (warm), {:.1}% (cold)",
        100.0 * warm / render,
        100.0 * cold / render
    );
    println!(
        "  per icon                      : {:.1} us",
        1_000.0 * warm / f64::from(u32::try_from(discovery.resources.len().max(1)).unwrap_or(1))
    );
    if !cfg!(debug_assertions) {
        return;
    }
    eprintln!("note: built without --release; these numbers are not usable");
}
