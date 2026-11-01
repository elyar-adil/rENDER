//! Live scrolling benchmark: retained compositor vs complete raster.
//!
//! Run with:
//! `cargo run --release -p render-browser --bin render-scroll-bench`
//!
//! The fixture is a long document with ordinary content and one translated
//! opacity layer. The benchmark warms the compositor, then scrolls the same
//! scene through both paths and prints a JSON result. The retained numbers are
//! the ones that matter: after the first frame, a 16px scroll should rasterize
//! no new bands.

#![allow(
    clippy::cast_precision_loss,
    reason = "benchmark fixture indices and frame counts are small exact integers"
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use render_core::dom::DomRevision;
use render_core::layout::{FragmentId, PhysicalPoint, PhysicalRect, PhysicalSize};
use render_core::paint::{
    BlendMode, Color, CompositingReason, CompositorCache, CpuRasterizer, DisplayCommand,
    DisplayItem, DisplayItemId, DisplayList, NoGlyphMasks, PaintCoordinateSpace, PaintPhase,
    StackingContext, Transform2D,
};

const FRAMES: usize = 120;
const SCROLL_STEP: f32 = 16.0;
const VIEWPORT: PhysicalSize = PhysicalSize {
    width: 800.0,
    height: 600.0,
};

fn item(index: u32, bounds: PhysicalRect, command: DisplayCommand) -> DisplayItem {
    DisplayItem {
        id: DisplayItemId {
            source: None,
            fragment_hint: index,
            phase: PaintPhase::Background,
            ordinal: index,
        },
        fragment: FragmentId::from_index(index as usize),
        source: None,
        bounds,
        coordinate_space: PaintCoordinateSpace::Document,
        command,
    }
}

fn rect(x: f32, y: f32, width: f32, height: f32, color: Color) -> DisplayCommand {
    DisplayCommand::SolidRect {
        rect: PhysicalRect::new(x, y, width, height),
        color,
    }
}

fn scene() -> DisplayList {
    let mut items = Vec::new();
    let mut ordinal = 0_u32;
    items.push(item(
        ordinal,
        PhysicalRect::new(0.0, 0.0, 1000.0, 12_000.0),
        rect(0.0, 0.0, 1000.0, 12_000.0, Color::rgb(250, 250, 250)),
    ));
    ordinal += 1;
    for index in 0..160 {
        let y = 80.0 + index as f32 * 72.0;
        let color = Color::rgb(
            u8::try_from((index * 37) % 240).unwrap_or(0),
            u8::try_from((index * 71) % 240).unwrap_or(0),
            u8::try_from((index * 113) % 240).unwrap_or(0),
        );
        items.push(item(
            ordinal,
            PhysicalRect::new(40.0, y, 640.0, 48.0),
            rect(40.0, y, 640.0, 48.0, color),
        ));
        ordinal += 1;
    }
    // One translated, half-opacity layer, the compositor shape the browser
    // treats as a separate retained surface.
    items.push(item(
        ordinal,
        PhysicalRect::new(0.0, 0.0, 300.0, 200.0),
        DisplayCommand::PushStackingContext(StackingContext {
            opacity: 0.5,
            transform: Transform2D {
                translate_x: 100.0,
                translate_y: 40.0,
                ..Transform2D::default()
            },
            blend_mode: BlendMode::Normal,
            isolated: true,
            reason: CompositingReason::Transform,
            filter: None,
            mask: None,
        }),
    ));
    ordinal += 1;
    items.push(item(
        ordinal,
        PhysicalRect::new(0.0, 0.0, 300.0, 200.0),
        rect(0.0, 0.0, 300.0, 200.0, Color::rgb(20, 120, 220)),
    ));
    ordinal += 1;
    items.push(item(
        ordinal,
        PhysicalRect::new(0.0, 0.0, 300.0, 200.0),
        DisplayCommand::PopStackingContext,
    ));
    DisplayList::from_items(DomRevision::default(), VIEWPORT, items)
}

fn main() {
    let display = Arc::new(scene());
    let compositor = CompositorCache::from_display_list(Arc::clone(&display), Color::WHITE)
        .expect("the benchmark scene is compositable");
    // Warm the first frame so the measurement is scroll, not initial paint.
    let _ = compositor.render(PhysicalPoint::default(), &NoGlyphMasks, None, None);
    let initial = compositor.stats();

    let scroll_started = Instant::now();
    for frame in 0..FRAMES {
        let offset = PhysicalPoint {
            x: 0.0,
            y: frame as f32 * SCROLL_STEP,
        };
        let output = compositor.render(offset, &NoGlyphMasks, None, None);
        std::hint::black_box(output.surface.pixel(0, 0));
    }
    let compositor_elapsed = scroll_started.elapsed();
    let final_stats = compositor.stats();

    let full_started = Instant::now();
    for frame in 0..FRAMES {
        let offset = PhysicalPoint {
            x: 0.0,
            y: frame as f32 * SCROLL_STEP,
        };
        let output =
            CpuRasterizer.rasterize_viewport(&display, Color::WHITE, &NoGlyphMasks, offset);
        std::hint::black_box(output.surface.pixel(0, 0));
    }
    let full_elapsed = full_started.elapsed();

    let mut json = HashMap::new();
    json.insert("frames", FRAMES.to_string());
    json.insert("scroll_step_px", SCROLL_STEP.to_string());
    json.insert(
        "viewport",
        format!("{}x{}", VIEWPORT.width, VIEWPORT.height),
    );
    json.insert(
        "compositor_ms",
        format!("{:.3}", compositor_elapsed.as_secs_f64() * 1_000.0),
    );
    json.insert(
        "compositor_us_per_frame",
        format!(
            "{:.1}",
            compositor_elapsed.as_secs_f64() * 1_000_000.0 / FRAMES as f64
        ),
    );
    json.insert(
        "full_raster_ms",
        format!("{:.3}", full_elapsed.as_secs_f64() * 1_000.0),
    );
    json.insert(
        "full_raster_us_per_frame",
        format!(
            "{:.1}",
            full_elapsed.as_secs_f64() * 1_000_000.0 / FRAMES as f64
        ),
    );
    json.insert("initial_bands", initial.bands_rasterized.to_string());
    json.insert("final_bands", final_stats.bands_rasterized.to_string());
    json.insert(
        "scroll_bands_rasterized",
        (final_stats.bands_rasterized - initial.bands_rasterized).to_string(),
    );
    json.insert(
        "scroll_frames_without_new_bands",
        (FRAMES as u64)
            .saturating_sub(
                final_stats
                    .bands_rasterized
                    .saturating_sub(initial.bands_rasterized),
            )
            .to_string(),
    );
    let keys = [
        "frames",
        "scroll_step_px",
        "viewport",
        "compositor_ms",
        "compositor_us_per_frame",
        "full_raster_ms",
        "full_raster_us_per_frame",
        "initial_bands",
        "final_bands",
        "scroll_bands_rasterized",
        "scroll_frames_without_new_bands",
    ];
    let fields = keys
        .iter()
        .map(|key| format!("\"{key}\":\"{}\"", json[key]))
        .collect::<Vec<_>>();
    println!("{{{}}}", fields.join(","));
}
