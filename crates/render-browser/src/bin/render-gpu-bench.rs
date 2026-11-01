//! Live GPU presentation benchmark for the wgpu/D3D12 swapchain path.
//!
//! Run with:
//! `cargo run --release -p render-browser --bin render-gpu-bench`
//!
//! The benchmark creates a hidden winit window, builds a real retained scroll
//! scene, and presents 120 scrolled surfaces through [`GpuPresenter`]. The
//! reported present time covers the page blit, the ARGB-to-RGBA conversion,
//! the padded texture upload, the fullscreen-triangle draw, and
//! `queue.present` followed by a GPU wait. No event-loop frame pacing is
//! involved: the measurement is the worker's presentation call.
//!
//! The JSON result names the adapter and backend and reports whether every
//! frame reached the swapchain. A machine without a usable adapter or window
//! prints an error and exits nonzero.

#![allow(
    clippy::cast_precision_loss,
    reason = "window dimensions and benchmark frame counts are small exact integers"
)]

use std::sync::Arc;

use render_browser::compositor::PlatformPresenter;
use render_browser::gpu_presenter::GpuPresenter;
use render_core::dom::DomRevision;
use render_core::layout::{FragmentId, PhysicalPoint, PhysicalRect, PhysicalSize};
use render_core::paint::{
    Color, CompositorCache, DisplayCommand, DisplayItem, DisplayItemId, DisplayList, NoGlyphMasks,
    PaintCoordinateSpace, PaintPhase,
};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize as WindowSize;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::Window;

const FRAMES: usize = 120;
const SCROLL_STEP: f32 = 16.0;
const CHROME_HEIGHT: u32 = 40;
const WINDOW_WIDTH: u32 = 1000;
const WINDOW_HEIGHT: u32 = 740;

struct Report {
    adapter: String,
    backend: String,
    window: String,
    presented_frames: u64,
    all_presented: bool,
    present_us: Vec<f64>,
    compositor_us: Vec<f64>,
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

fn scene(viewport: PhysicalSize) -> DisplayList {
    let mut items = Vec::new();
    let mut ordinal = 0_u32;
    items.push(item(
        ordinal,
        PhysicalRect::new(0.0, 0.0, viewport.width, 6000.0),
        rect(0.0, 0.0, viewport.width, 6000.0, Color::rgb(250, 250, 250)),
    ));
    ordinal += 1;
    for index in 0..120 {
        let y = 60.0 + index as f32 * 48.0;
        let color = Color::rgb(
            u8::try_from((index * 37) % 240).unwrap_or(0),
            u8::try_from((index * 71) % 240).unwrap_or(0),
            u8::try_from((index * 113) % 240).unwrap_or(0),
        );
        items.push(item(
            ordinal,
            PhysicalRect::new(40.0, y, viewport.width - 80.0, 32.0),
            rect(40.0, y, viewport.width - 80.0, 32.0, color),
        ));
        ordinal += 1;
    }
    // A fixed header exercises a viewport-space retained layer, so the scene
    // is not only document-space rectangles.
    let mut fixed = item(
        ordinal,
        PhysicalRect::new(0.0, 0.0, viewport.width, 32.0),
        rect(0.0, 0.0, viewport.width, 32.0, Color::rgb(15, 80, 160)),
    );
    fixed.coordinate_space = PaintCoordinateSpace::Viewport;
    items.push(fixed);
    DisplayList::from_items(DomRevision::default(), viewport, items)
}

fn run(event_loop: &ActiveEventLoop) -> Result<Report, String> {
    let attributes = Window::default_attributes()
        .with_title("rENDER GPU presenter benchmark")
        .with_inner_size(WindowSize::new(WINDOW_WIDTH, WINDOW_HEIGHT))
        .with_visible(false)
        .with_active(false);
    let window = Arc::new(
        event_loop
            .create_window(attributes)
            .map_err(|error| format!("create the benchmark window: {error}"))?,
    );
    let size = window.inner_size();
    if size.width == 0 || size.height <= CHROME_HEIGHT {
        return Err(format!(
            "the benchmark window has no usable size ({}x{})",
            size.width, size.height
        ));
    }
    let mut presenter = GpuPresenter::new(Arc::clone(&window))?;
    let adapter = presenter.adapter_name().to_owned();
    let backend = presenter.backend().to_owned();
    let viewport = PhysicalSize {
        width: size.width as f32,
        height: (size.height - CHROME_HEIGHT) as f32,
    };
    let display = Arc::new(scene(viewport));
    let compositor = CompositorCache::from_display_list(display, Color::WHITE)
        .ok_or_else(|| "the benchmark scene is not compositable".to_owned())?;
    let full_frame =
        vec![
            0x00f0_f0f0;
            usize::try_from(size.width)
                .ok()
                .and_then(|width| {
                    usize::try_from(size.height)
                        .ok()
                        .and_then(|height| width.checked_mul(height))
                })
                .ok_or_else(|| "the benchmark window area overflows memory".to_owned())?
        ];
    presenter.present_full(&full_frame, size.width, size.height)?;
    // Warm the band cache and the upload staging buffer before measuring.
    let warm = compositor.render(PhysicalPoint::default(), &NoGlyphMasks, None, None);
    presenter.present_page(&warm.surface, 0, CHROME_HEIGHT)?;
    let presented_before = presenter.presented_frames();

    let mut present_us = Vec::with_capacity(FRAMES);
    let mut compositor_us = Vec::with_capacity(FRAMES);
    for frame in 0..FRAMES {
        let offset = PhysicalPoint {
            x: 0.0,
            y: frame as f32 * SCROLL_STEP,
        };
        let render_started = std::time::Instant::now();
        let output = compositor.render(offset, &NoGlyphMasks, None, None);
        compositor_us.push(render_started.elapsed().as_secs_f64() * 1_000_000.0);
        let present = presenter.present_page(&output.surface, 0, CHROME_HEIGHT)?;
        present_us.push(present.as_secs_f64() * 1_000_000.0);
    }
    let presented_frames = presenter
        .presented_frames()
        .saturating_sub(presented_before);
    let expected = u64::try_from(FRAMES).expect("the frame count fits in u64");
    Ok(Report {
        adapter,
        backend,
        window: format!("{}x{}", size.width, size.height),
        presented_frames,
        all_presented: presented_frames == expected,
        present_us,
        compositor_us,
    })
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        f64::midpoint(sorted[middle - 1], sorted[middle])
    } else {
        sorted[middle]
    }
}

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn print_report(report: &Report) {
    let maximum = report.present_us.iter().copied().fold(0.0, f64::max);
    println!(
        concat!(
            "{{\"frames\":{},\"adapter\":\"{}\",\"backend\":\"{}\",",
            "\"window\":\"{}\",\"presented_frames\":{},\"all_presented\":{},",
            "\"mean_present_us\":{:.1},\"median_present_us\":{:.1},\"max_present_us\":{:.1},",
            "\"mean_compositor_us\":{:.1}}}"
        ),
        report.present_us.len(),
        json_escape(&report.adapter),
        json_escape(&report.backend),
        json_escape(&report.window),
        report.presented_frames,
        report.all_presented,
        mean(&report.present_us),
        median(&report.present_us),
        maximum,
        mean(&report.compositor_us),
    );
}

struct BenchApp {
    report: Option<Result<Report, String>>,
}

impl ApplicationHandler for BenchApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.report.is_some() {
            return;
        }
        self.report = Some(run(event_loop));
        event_loop.exit();
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _event: winit::event::WindowEvent,
    ) {
        // The benchmark presents directly from `resumed`; it never needs a
        // window event.
    }
}

fn main() {
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("render-gpu-bench: create event loop: {error}");
            std::process::exit(1);
        }
    };
    let mut app = BenchApp { report: None };
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("render-gpu-bench: event loop failed: {error}");
        std::process::exit(1);
    }
    match app.report {
        Some(Ok(report)) => print_report(&report),
        Some(Err(error)) => {
            eprintln!("render-gpu-bench: {error}");
            std::process::exit(1);
        }
        None => {
            eprintln!("render-gpu-bench: the event loop stopped before creating a window");
            std::process::exit(1);
        }
    }
}
