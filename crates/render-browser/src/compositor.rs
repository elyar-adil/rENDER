//! A dedicated scroll-compositing worker.
//!
//! The render worker owns style, layout, display-list construction and the
//! complete raster. A scroll frame needs none of those: the retained
//! [`CompositorCache`] already holds the scene, and composing a new viewport is
//! a translation plus a small amount of band rasterization. Running that work
//! on the render worker's queue would make a scroll wait behind an unrelated
//! full render, which is exactly the coupling this module removes.
//!
//! The worker deliberately knows nothing about winit or a native surface. It
//! returns a CPU [`Surface`]; the browser shell converts it to its presentable
//! buffer and never re-enters layout. This is the CPU dedicated-surface
//! implementation: there is no GPU context yet, but the scheduling boundary is
//! the one a GPU compositor would use.
//!
//! One worker is created per browser process. Root segments, affine layers and
//! the band cache are shared through `Arc<CompositorCache>`; the cache's own
//! mutex makes concurrent animation frames and scroll frames safe.

use std::sync::Arc;
use std::time::{Duration, Instant};

use render_core::image::ImageResources;
use render_core::layout::PhysicalPoint;
use render_core::paint::{CompositorCache, GlyphMaskProvider, RasterControl, Surface};

use crate::font_backend::SystemFontBackend;
use crate::worker::{
    RenderCancellation, RenderJob, RenderWorker, RenderWorkerOptions, RenderWorkerStartError,
};

/// The input of one scroll-compositing job.
///
/// The scroll position travels in [`render_core::worker::RenderIdentity`], so a
/// job and its completion share the same identity the rest of the pipeline
/// uses for supersession and coalescing. Everything else is a cheap clone of
/// state that already belongs to the page.
#[derive(Clone, Debug)]
pub struct CompositorScroll {
    pub compositor: Arc<CompositorCache>,
    pub images: ImageResources,
    pub content_height: f32,
    pub viewport_height: f32,
}

/// One completed scroll frame.
#[derive(Debug)]
pub struct CompositorFrame {
    pub surface: Surface,
    /// Time spent inside the compositor worker for this frame.
    pub render_time: Duration,
    /// Thread that executed the frame. Used by the independent-thread test and
    /// useful in frame logs.
    pub worker_thread: std::thread::ThreadId,
    pub content_height: f32,
    pub viewport_height: f32,
}

/// The browser's dedicated scroll-compositing queue.
pub type CompositorWorker = RenderWorker<CompositorScroll, CompositorFrame>;

/// Starts the dedicated compositor worker.
///
/// `wake` is called after a frame is queued for collection, exactly like the
/// render worker's wake callback, so the event loop never polls on a timer.
///
/// # Errors
///
/// Returns the render worker's configuration or spawn error.
pub fn start_compositor_worker<W>(
    fonts: Arc<SystemFontBackend>,
    wake: W,
) -> Result<CompositorWorker, RenderWorkerStartError>
where
    W: Fn() + Send + Sync + 'static,
{
    RenderWorker::start(
        RenderWorkerOptions {
            // A page has one outstanding scroll frame at a time: a newer
            // scroll supersedes the older one. A small queue keeps a burst of
            // wheel events from retaining old surfaces.
            queue_capacity: 8,
            worker_count: 1,
        },
        move |job: RenderJob<CompositorScroll>, cancellation: &RenderCancellation| {
            let started = Instant::now();
            let control = WorkerRasterControl { cancellation };
            let CompositorScroll {
                compositor,
                images,
                content_height,
                viewport_height,
            } = job.payload;
            let output = compositor.render(
                PhysicalPoint {
                    x: job.identity.scroll_offset.x,
                    y: job.identity.scroll_offset.y,
                },
                fonts.as_ref() as &dyn GlyphMaskProvider,
                Some(&images),
                Some(&control),
            );
            cancellation.check()?;
            Ok(CompositorFrame {
                surface: output.surface,
                render_time: started.elapsed(),
                worker_thread: std::thread::current().id(),
                content_height,
                viewport_height,
            })
        },
        wake,
    )
}

struct WorkerRasterControl<'a> {
    cancellation: &'a RenderCancellation,
}

impl RasterControl for WorkerRasterControl<'_> {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use render_core::dom::DomRevision;
    use render_core::image::ImageResources;
    use render_core::layout::{FragmentId, PhysicalRect, PhysicalSize};
    use render_core::paint::{
        Color, CompositorCache, DisplayCommand, DisplayItem, DisplayItemId, DisplayList,
        PaintCoordinateSpace, PaintPhase,
    };

    use super::{CompositorScroll, start_compositor_worker};
    use crate::font_backend::SystemFontBackend;
    use crate::worker::{RenderIdentity, RenderJob, RenderOffset, RenderViewport};

    fn compositor() -> Arc<CompositorCache> {
        let viewport = PhysicalSize {
            width: 80.0,
            height: 60.0,
        };
        let bounds = PhysicalRect::new(0.0, 0.0, 80.0, 160.0);
        let item = DisplayItem {
            id: DisplayItemId {
                source: None,
                fragment_hint: 0,
                phase: PaintPhase::Background,
                ordinal: 0,
            },
            fragment: FragmentId::from_index(0),
            source: None,
            bounds,
            coordinate_space: PaintCoordinateSpace::Document,
            command: DisplayCommand::SolidRect {
                rect: bounds,
                color: Color::WHITE,
            },
        };
        let list = DisplayList::from_items(DomRevision::default(), viewport, vec![item]);
        Arc::new(
            CompositorCache::from_display_list(Arc::new(list), Color::WHITE)
                .expect("the fixture is compositable"),
        )
    }

    fn identity(generation: u64, scroll_y: f32) -> RenderIdentity {
        RenderIdentity {
            tab_id: 1,
            generation,
            dom_revision: 1,
            viewport: RenderViewport {
                width: 80,
                height: 60,
            },
            scroll_offset: RenderOffset {
                x: 0.0,
                y: scroll_y,
            },
            external_styles_generation: 0,
            animation_time_millis: 0,
        }
    }

    /// The worker must run on its own thread and return a composed surface for
    /// each scroll position. Twenty sequential frames exercise the queue and
    /// the per-frame timing path; the generous upper bound catches a frame
    /// accidentally falling back to layout-scale work.
    #[test]
    fn twenty_scroll_frames_run_on_the_dedicated_thread() {
        let fonts = Arc::new(SystemFontBackend::load().expect("platform fonts load"));
        let (sender, receiver) = mpsc::channel();
        let worker = start_compositor_worker(Arc::clone(&fonts), move || {
            let _ = sender.send(());
        })
        .expect("the compositor worker starts");
        let cache = compositor();
        let caller = std::thread::current().id();
        let mut worst = Duration::ZERO;
        for generation in 1..=20 {
            let scroll_y = f32::from(u16::try_from(generation * 4).expect("small fixture"));
            worker
                .submit(RenderJob {
                    identity: identity(generation, scroll_y),
                    source_snapshot: Arc::from("fixture"),
                    payload: CompositorScroll {
                        compositor: Arc::clone(&cache),
                        images: ImageResources::default(),
                        content_height: 160.0,
                        viewport_height: 60.0,
                    },
                })
                .expect("the worker accepts a scroll job");
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut frame = None;
            while frame.is_none() {
                assert!(
                    Instant::now() < deadline,
                    "scroll frame {generation} never completed"
                );
                if receiver.recv_timeout(Duration::from_millis(50)).is_ok() {
                    for completed in worker.drain_latest() {
                        frame = completed.result.ok();
                    }
                }
            }
            let frame = frame.expect("a frame was produced");
            assert_ne!(
                frame.worker_thread, caller,
                "the frame must not be composed on the caller's thread"
            );
            assert_eq!(frame.surface.width(), 80);
            assert_eq!(frame.surface.height(), 60);
            assert_eq!(frame.surface.pixel(5, 5), Some(Color::WHITE));
            worst = worst.max(frame.render_time);
        }
        assert!(
            worst < Duration::from_secs(2),
            "a 80x60 scroll frame took {worst:?}"
        );
        worker.shutdown();
    }
}
