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
//! returns a CPU [`Surface`] and hands it to an installed [`PlatformPresenter`],
//! which may copy it to softbuffer or upload it to a wgpu swapchain. Layout is
//! never re-entered from this path.
//!
//! One worker is created per browser process. Root segments, affine layers and
//! the band cache are shared through `Arc<CompositorCache>`; the cache's own
//! mutex makes concurrent animation frames and scroll frames safe.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use render_core::image::ImageResources;
use render_core::layout::PhysicalPoint;
use render_core::paint::{CompositorCache, GlyphMaskProvider, RasterControl, Surface};

use crate::font_backend::SystemFontBackend;
use crate::worker::{
    RenderCancellation, RenderJob, RenderWorker, RenderWorkerOptions, RenderWorkerStartError,
};

/// A platform surface the compositor can write into directly.
///
/// The browser shell implements this over its native window buffer. The trait
/// deliberately uses only CPU frame data and a [`Surface`], so the compositor
/// worker does not need a winit or softbuffer type in this crate. A future GPU
/// backend can implement the same two methods with a texture upload and a
/// swapchain present.
pub trait PlatformPresenter: Send {
    /// Presents a complete window frame, including browser chrome.
    ///
    /// # Errors
    ///
    /// Returns the platform's failure text when the surface cannot be resized
    /// or presented.
    fn present_full(&mut self, frame: &[u32], width: u32, height: u32) -> Result<(), String>;

    /// Composites one page surface at `origin` into the last full frame and
    /// presents the window without touching the chrome.
    ///
    /// Returns the time spent in the platform present call on success.
    ///
    /// # Errors
    ///
    /// Returns the platform's failure text when no full frame has established
    /// the base surface yet, or when the present itself fails.
    fn present_page(
        &mut self,
        page: &Surface,
        origin_x: u32,
        origin_y: u32,
    ) -> Result<Duration, String>;
}

/// A handle shared by the browser shell and the compositor worker.
///
/// The slot is installed after the window exists and emptied when the window is
/// destroyed. The compositor worker can therefore start before the window and
/// still present scroll frames directly once the platform surface is ready.
pub type SharedPresenter = Arc<Mutex<Option<Box<dyn PlatformPresenter>>>>;

#[must_use]
pub fn shared_presenter() -> SharedPresenter {
    Arc::new(Mutex::new(None))
}

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
    /// Top-left corner of the page region in the window buffer. The browser
    /// chrome occupies the rows above it.
    pub page_origin: (u32, u32),
}

/// One completed scroll frame.
#[derive(Debug)]
pub struct CompositorFrame {
    pub surface: Surface,
    /// Time spent inside the compositor worker for this frame.
    pub render_time: Duration,
    /// Time spent in the platform present call, when the worker presented
    /// directly. `None` means the shell still has to present the surface.
    pub present_time: Option<Duration>,
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
    presenter: SharedPresenter,
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
                page_origin,
            } = job.payload;
            // The retained cache is a physical-pixel scene; the identity's
            // scroll offset stays in CSS pixels like every other page metric.
            let device_scale =
                if job.identity.device_scale.is_finite() && job.identity.device_scale > 0.0 {
                    job.identity.device_scale
                } else {
                    1.0
                };
            let output = compositor.render(
                PhysicalPoint {
                    x: job.identity.scroll_offset.x * device_scale,
                    y: job.identity.scroll_offset.y * device_scale,
                },
                fonts.as_ref() as &dyn GlyphMaskProvider,
                Some(&images),
                Some(&control),
            );
            cancellation.check()?;
            let render_time = started.elapsed();
            let present_time = present_page(&presenter, &output.surface, page_origin);
            Ok(CompositorFrame {
                surface: output.surface,
                render_time,
                present_time,
                worker_thread: std::thread::current().id(),
                content_height,
                viewport_height,
            })
        },
        wake,
    )
}

/// Presents a page surface through the installed platform sink.
///
/// A missing sink, or a platform error, is not a frame failure: the surface is
/// still returned to the shell, which can use its ordinary redraw path. That
/// keeps a headless test and a temporary surface loss from breaking scrolling.
fn present_page(
    presenter: &SharedPresenter,
    page: &Surface,
    origin: (u32, u32),
) -> Option<Duration> {
    let mut presenter = presenter.lock().unwrap_or_else(PoisonError::into_inner);
    presenter
        .as_mut()?
        .present_page(page, origin.0, origin.1)
        .ok()
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
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use render_core::dom::DomRevision;
    use render_core::image::ImageResources;
    use render_core::layout::{FragmentId, PhysicalRect, PhysicalSize};
    use render_core::paint::{
        Color, CompositorCache, DisplayCommand, DisplayItem, DisplayItemId, DisplayList,
        PaintCoordinateSpace, PaintPhase, Surface,
    };

    use super::{
        CompositorScroll, PlatformPresenter, SharedPresenter, shared_presenter,
        start_compositor_worker,
    };
    use crate::font_backend::SystemFontBackend;
    use crate::worker::{RenderIdentity, RenderJob, RenderOffset, RenderViewport};

    #[derive(Default)]
    struct RecordingPresenter {
        base: Vec<u32>,
        width: u32,
        height: u32,
        presents: Arc<Mutex<Vec<(u32, u32)>>>,
    }

    impl PlatformPresenter for RecordingPresenter {
        fn present_full(&mut self, frame: &[u32], width: u32, height: u32) -> Result<(), String> {
            self.base = frame.to_vec();
            self.width = width;
            self.height = height;
            Ok(())
        }

        fn present_page(
            &mut self,
            page: &Surface,
            origin_x: u32,
            origin_y: u32,
        ) -> Result<Duration, String> {
            let started = Instant::now();
            if self.width == 0 || self.height == 0 {
                return Err("no full frame has been presented yet".to_owned());
            }
            for row in 0..page.height() {
                for column in 0..page.width() {
                    let x = origin_x + column;
                    let y = origin_y + row;
                    if x >= self.width || y >= self.height {
                        continue;
                    }
                    let color = page.pixel(column, row).unwrap_or(Color::TRANSPARENT);
                    let index = (y * self.width + x) as usize;
                    self.base[index] = (u32::from(color.red) << 16)
                        | (u32::from(color.green) << 8)
                        | u32::from(color.blue);
                }
            }
            self.presents
                .lock()
                .expect("present log lock")
                .push((origin_x, origin_y));
            Ok(started.elapsed())
        }
    }

    fn install_presenter(presenter: &SharedPresenter, log: Arc<Mutex<Vec<(u32, u32)>>>) {
        let mut slot = presenter.lock().expect("presenter slot");
        *slot = Some(Box::new(RecordingPresenter {
            base: Vec::new(),
            width: 0,
            height: 0,
            presents: log,
        }));
    }

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
            document_serial: 1,
            dom_revision: 1,
            viewport: RenderViewport {
                width: 80,
                height: 60,
            },
            scroll_offset: RenderOffset {
                x: 0.0,
                y: scroll_y,
            },
            device_scale: 1.0,
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
        let presenter = shared_presenter();
        let presents = Arc::new(Mutex::new(Vec::new()));
        install_presenter(&presenter, Arc::clone(&presents));
        presenter
            .lock()
            .expect("presenter slot")
            .as_mut()
            .expect("presenter installed")
            .present_full(&vec![0x00ff_ffff; 80 * 70], 80, 70)
            .expect("the base window frame is presentable");
        let (sender, receiver) = mpsc::channel();
        let worker =
            start_compositor_worker(Arc::clone(&fonts), Arc::clone(&presenter), move || {
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
                        page_origin: (0, 10),
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
            assert!(
                frame.present_time.is_some(),
                "the worker must present through the platform sink"
            );
            assert_eq!(frame.surface.width(), 80);
            assert_eq!(frame.surface.height(), 60);
            assert_eq!(frame.surface.pixel(5, 5), Some(Color::WHITE));
            worst = worst.max(frame.render_time);
        }
        assert_eq!(
            presents.lock().expect("present log").len(),
            20,
            "every scroll frame reaches the platform surface"
        );
        assert_eq!(
            presents.lock().expect("present log")[0],
            (0, 10),
            "the page is composited below the browser chrome"
        );
        assert!(
            worst < Duration::from_secs(2),
            "a 80x60 scroll frame took {worst:?}"
        );
        worker.shutdown();
    }
}
