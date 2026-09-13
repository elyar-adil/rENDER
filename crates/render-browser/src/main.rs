//! Native browser shell for the self-owned Rust rendering pipeline.
#![allow(clippy::cast_precision_loss)]
mod app;
mod content_interaction;
mod diagnostics;
mod fetch_handles;
mod frame;
mod page_source;
mod page_state;
mod render_worker;

#[cfg(test)]
mod app_tests;

use crate::app::BrowserApp;
use crate::page_source::load_initial_page;
use crate::render_worker::start_render_worker;
use render_browser::font_backend::SystemFontBackend;
use render_net::FetchConfig;
use render_net::HttpTransport;
use render_net::NetworkWorker;
use softbuffer::Surface as WindowSurface;
use std::sync::Arc;
use winit::event_loop::ControlFlow;
use winit::event_loop::EventLoop;
use winit::window::Window;

const INITIAL_WIDTH: u32 = 1_180;

const INITIAL_HEIGHT: u32 = 780;

const SCROLL_LINE_PIXELS: f32 = 40.0;

const ACTIVE_PAGE_TURN_BUDGET: usize = 8;

const BACKGROUND_PAGE_TURN_BUDGET: usize = 2;

type NativeSurface = WindowSurface<Arc<Window>, Arc<Window>>;

#[derive(Clone, Copy, Debug)]
enum UserEvent {
    RenderReady,
}

fn main() {
    #[cfg(target_os = "macos")]
    {
        if let Err(message) = browser_main() {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        // The interpreter and parser recurse deeply on minified real-world
        // scripts; the default main-thread stack overflows. Run the entire
        // event loop on a dedicated thread with a generous stack.
        let child = std::thread::Builder::new()
            .stack_size(512 * 1024 * 1024)
            .spawn(browser_main)
            .expect("spawn browser main thread");
        match child.join() {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                eprintln!("error: {message}");
                std::process::exit(1);
            }
            Err(panic_payload) => std::panic::resume_unwind(panic_payload),
        }
    }
}

fn browser_main() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    use winit::platform::windows::EventLoopBuilderExtWindows as _;
    let Some(initial) = load_initial_page().map_err(|error| error.to_string())? else {
        return Ok(());
    };
    let fonts = Arc::new(SystemFontBackend::load().map_err(|error| error.to_string())?);
    let event_loop = {
        let mut builder = EventLoop::<UserEvent>::with_user_event();
        // The event loop lives on our dedicated big-stack thread for the
        // whole program lifetime; no other thread touches it.
        #[cfg(target_os = "windows")]
        builder.with_any_thread(true);
        builder.build().map_err(|error| error.to_string())?
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    let network = NetworkWorker::start(HttpTransport::new(FetchConfig::default()))
        .map_err(|error| error.to_string())?;
    let render_worker = start_render_worker(Arc::clone(&fonts), event_loop.create_proxy())
        .map_err(|error| error.to_string())?;
    let mut app = BrowserApp::new(initial, fonts, network, render_worker);
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())?;
    Ok(())
}
