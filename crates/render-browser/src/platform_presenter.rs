//! The native-window implementation of the compositor's platform presenter.
//!
//! This shell still presents through `softbuffer`, which writes a CPU buffer to
//! the window's platform surface. The important architectural property is the
//! thread, not the raster API: the compositor worker calls this sink directly,
//! so a scroll frame reaches the window without returning through the render
//! worker or waiting for the event-loop thread. A GPU swapchain can implement
//! the same [`PlatformPresenter`] contract later without changing the scheduler.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use render_browser::compositor::PlatformPresenter;
use render_core::paint::Surface;
use softbuffer::{Context, Surface as WindowSurface};
use winit::window::Window;

use crate::frame::surface_to_softbuffer;

/// Presents composed frames to the winit window through softbuffer.
///
/// `base` is the last complete window frame, including chrome. A scroll frame
/// replaces only its page rectangle and presents the whole base, so the
/// compositor worker never has to know how the chrome was painted.
pub(super) struct SoftbufferPresenter {
    surface: WindowSurface<Arc<Window>, Arc<Window>>,
    base: Vec<u32>,
    width: u32,
    height: u32,
}

impl SoftbufferPresenter {
    pub(super) fn new(context: &Context<Arc<Window>>, window: Arc<Window>) -> Result<Self, String> {
        let surface = WindowSurface::new(context, window).map_err(|error| error.to_string())?;
        Ok(Self {
            surface,
            base: Vec::new(),
            width: 0,
            height: 0,
        })
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        let width = NonZeroU32::new(width).ok_or_else(|| "zero-width frame".to_owned())?;
        let height = NonZeroU32::new(height).ok_or_else(|| "zero-height frame".to_owned())?;
        self.surface
            .resize(width, height)
            .map_err(|error| error.to_string())
    }

    fn write_base(&mut self) -> Result<(), String> {
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|error| error.to_string())?;
        if buffer.len() != self.base.len() {
            return Err("native surface and base frame sizes differ".to_owned());
        }
        buffer.copy_from_slice(&self.base);
        buffer.present().map_err(|error| error.to_string())
    }
}

impl PlatformPresenter for SoftbufferPresenter {
    fn present_full(&mut self, frame: &[u32], width: u32, height: u32) -> Result<(), String> {
        self.resize(width, height)?;
        self.base.clear();
        self.base.extend_from_slice(frame);
        self.width = width;
        self.height = height;
        self.write_base()
    }

    fn present_page(
        &mut self,
        page: &Surface,
        origin_x: u32,
        origin_y: u32,
    ) -> Result<Duration, String> {
        let started = Instant::now();
        if self.width == 0 || self.height == 0 || self.base.is_empty() {
            return Err("no full window frame has been presented yet".to_owned());
        }
        let page_pixels = surface_to_softbuffer(page);
        let page_width = page.width() as usize;
        let copy_width = page_width.min((self.width.saturating_sub(origin_x)) as usize);
        let copy_height =
            (page.height() as usize).min((self.height.saturating_sub(origin_y)) as usize);
        for row in 0..copy_height {
            let source_start = row * page_width;
            let destination_start =
                (origin_y as usize + row) * self.width as usize + origin_x as usize;
            self.base[destination_start..destination_start + copy_width]
                .copy_from_slice(&page_pixels[source_start..source_start + copy_width]);
        }
        self.write_base()?;
        Ok(started.elapsed())
    }
}
