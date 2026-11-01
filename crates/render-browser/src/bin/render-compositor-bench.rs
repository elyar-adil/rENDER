//! Live proof that scroll composition does not wait on the render worker.
//!
//! Run with:
//! `cargo run --release -p render-browser --bin render-compositor-bench`
//!
//! A simulated render worker is blocked for 500 ms, then the benchmark submits
//! 120 sequential scroll jobs to the dedicated compositor worker. If the two
//! shared a queue, zero frames would complete before the block ends. The JSON
//! result reports how many frames landed during the block and the per-frame
//! compositor latency.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "benchmark fixture indices and viewport constants are small exact integers"
)]

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use render_browser::compositor::{CompositorScroll, start_compositor_worker};
use render_browser::font_backend::SystemFontBackend;
use render_browser::worker::{RenderIdentity, RenderJob, RenderOffset, RenderViewport};
use render_core::dom::DomRevision;
use render_core::image::ImageResources;
use render_core::layout::{FragmentId, PhysicalRect, PhysicalSize};
use render_core::paint::{
    Color, CompositorCache, DisplayCommand, DisplayItem, DisplayItemId, DisplayList, NoGlyphMasks,
    PaintCoordinateSpace, PaintPhase,
};

const FRAMES: usize = 120;
const SCROLL_STEP: f32 = 16.0;
const BLOCK_MILLIS: u64 = 500;
const VIEWPORT: PhysicalSize = PhysicalSize {
    width: 800.0,
    height: 600.0,
};

#[allow(
    clippy::too_many_lines,
    reason = "the benchmark fixture is one scene description; splitting it would hide the element mix being measured"
)]
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
    DisplayList::from_items(DomRevision::default(), VIEWPORT, items)
}

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

fn identity(generation: u64, scroll_y: f32) -> RenderIdentity {
    RenderIdentity {
        tab_id: 1,
        generation,
        dom_revision: 1,
        viewport: RenderViewport {
            width: VIEWPORT.width as u32,
            height: VIEWPORT.height as u32,
        },
        scroll_offset: RenderOffset {
            x: 0.0,
            y: scroll_y,
        },
        external_styles_generation: 0,
        animation_time_millis: 0,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the benchmark is one sequential submit/wait loop with its result formatting"
)]
fn main() {
    let fonts = Arc::new(SystemFontBackend::load().expect("platform fonts load"));
    let display = Arc::new(scene());
    let compositor = Arc::new(
        CompositorCache::from_display_list(Arc::clone(&display), Color::WHITE)
            .expect("the benchmark scene is compositable"),
    );
    // Warm the band cache so the measurement is scroll composition.
    let _ = compositor.render(
        render_core::layout::PhysicalPoint::default(),
        &NoGlyphMasks,
        None,
        None,
    );

    let (sender, receiver) = mpsc::channel();
    let worker = start_compositor_worker(Arc::clone(&fonts), move || {
        let _ = sender.send(());
    })
    .expect("the compositor worker starts");

    let barrier = Arc::new(Barrier::new(2));
    let (block_sender, block_receiver) = mpsc::channel();
    let blocked = {
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            block_sender.send(()).expect("the benchmark receiver lives");
            std::thread::sleep(Duration::from_millis(BLOCK_MILLIS));
        })
    };
    barrier.wait();
    block_receiver
        .recv()
        .expect("the simulated render worker starts");
    let block_begin = Instant::now();
    let block_end = block_begin + Duration::from_millis(BLOCK_MILLIS);

    let mut latencies = Vec::with_capacity(FRAMES);
    let mut frames_during_block = 0_usize;
    for frame in 0..FRAMES {
        let started = Instant::now();
        worker
            .submit(RenderJob {
                identity: identity(frame as u64 + 1, frame as f32 * SCROLL_STEP),
                source_snapshot: Arc::from("benchmark"),
                payload: CompositorScroll {
                    compositor: Arc::clone(&compositor),
                    images: ImageResources::default(),
                    content_height: 12_000.0,
                    viewport_height: VIEWPORT.height,
                },
            })
            .expect("the compositor worker accepts a scroll job");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut completed_frame = None;
        while completed_frame.is_none() {
            assert!(
                Instant::now() < deadline,
                "scroll frame {frame} never completed"
            );
            if receiver.recv_timeout(Duration::from_millis(50)).is_ok() {
                for completed in worker.drain_latest() {
                    if let Ok(result) = completed.result {
                        completed_frame = Some(result);
                    }
                }
            }
        }
        let completed_at = Instant::now();
        latencies.push(started.elapsed());
        if completed_at <= block_end {
            frames_during_block += 1;
        }
    }
    blocked
        .join()
        .expect("the simulated render worker finishes");
    worker.shutdown();

    latencies.sort_unstable();
    let total = latencies.iter().copied().sum::<Duration>();
    let mean = total.as_secs_f64() * 1_000_000.0 / FRAMES as f64;
    let median = latencies[FRAMES / 2].as_secs_f64() * 1_000_000.0;
    let worst = latencies[FRAMES - 1].as_secs_f64() * 1_000_000.0;

    let mut json = HashMap::new();
    json.insert("frames", FRAMES.to_string());
    json.insert("scroll_step_px", SCROLL_STEP.to_string());
    json.insert(
        "viewport",
        format!("{}x{}", VIEWPORT.width, VIEWPORT.height),
    );
    json.insert("render_worker_block_ms", BLOCK_MILLIS.to_string());
    json.insert("frames_during_block", frames_during_block.to_string());
    json.insert("mean_latency_us", format!("{mean:.1}"));
    json.insert("median_latency_us", format!("{median:.1}"));
    json.insert("max_latency_us", format!("{worst:.1}"));
    let keys = [
        "frames",
        "scroll_step_px",
        "viewport",
        "render_worker_block_ms",
        "frames_during_block",
        "mean_latency_us",
        "median_latency_us",
        "max_latency_us",
    ];
    let fields = keys
        .iter()
        .map(|key| format!("\"{key}\":\"{}\"", json[key]))
        .collect::<Vec<_>>();
    println!("{{{}}}", fields.join(","));
}
