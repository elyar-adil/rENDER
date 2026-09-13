//! Native browser shell for the self-owned Rust rendering pipeline.
#![allow(clippy::cast_precision_loss)]
use crate::render_worker::PageRenderFrame;
use render_browser::resources::StylesheetResourceDiagnostic;
use render_browser::scripts::ScriptResourceDiagnostic;
use render_core::script::ScriptDiagnostic;
use render_net::Url;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use winit::dpi::PhysicalSize as WindowSize;

pub(super) fn log_completed_frame_debug(frame: &PageRenderFrame, tab_id: u64) {
    if env::var_os("RENDER_DEBUG_FRAME").is_none() {
        return;
    }
    eprintln!(
        "render-browser frame page={tab_id} pixels={} display_items={} geometry={} content_height={} viewport_height={}",
        frame.frame.len(),
        frame
            .display_list
            .as_ref()
            .map_or(0, |list| list.items().len()),
        frame.geometry.as_ref().map_or(0, BTreeMap::len),
        frame.content_height,
        frame.viewport_height,
    );
    let Some(display_list) = &frame.display_list else {
        return;
    };
    for item in display_list.items().iter().take(24) {
        let command = match &item.command {
            render_core::paint::DisplayCommand::SolidRect { .. } => "solid",
            render_core::paint::DisplayCommand::Border(_) => "border",
            render_core::paint::DisplayCommand::BoxShadow(_) => "shadow",
            render_core::paint::DisplayCommand::PushClip(_) => "push-clip",
            render_core::paint::DisplayCommand::PopClip => "pop-clip",
            render_core::paint::DisplayCommand::PushTransform(_) => "push-transform",
            render_core::paint::DisplayCommand::PopTransform => "pop-transform",
            render_core::paint::DisplayCommand::GlyphRun(_) => "glyph",
            render_core::paint::DisplayCommand::TextDecoration(_) => "decoration",
            render_core::paint::DisplayCommand::Image(_) => "image",
            render_core::paint::DisplayCommand::LinearGradient(_) => "linear-gradient",
            render_core::paint::DisplayCommand::RadialGradient(_) => "radial-gradient",
            render_core::paint::DisplayCommand::Canvas { .. } => "canvas",
            render_core::paint::DisplayCommand::PushStackingContext(_) => "push-stack",
            render_core::paint::DisplayCommand::PopStackingContext => "pop-stack",
        };
        eprintln!(
            "render-browser display command={} source={:?} bounds={:?}",
            command, item.source, item.bounds
        );
    }
}

pub(super) fn dump_debug_frame(frame: &[u32], size: WindowSize<u32>) {
    let Some(path) = env::var_os("RENDER_DUMP_FRAME") else {
        return;
    };
    if size.width == 0
        || size.height == 0
        || frame.len() != size.width as usize * size.height as usize
    {
        return;
    }
    let mut ppm = format!("P6\n{} {}\n255\n", size.width, size.height).into_bytes();
    ppm.reserve(frame.len().saturating_mul(3));
    for pixel in frame {
        ppm.extend_from_slice(&[
            ((pixel >> 16) & 0xff) as u8,
            ((pixel >> 8) & 0xff) as u8,
            (pixel & 0xff) as u8,
        ]);
    }
    let _ = fs::write(path, ppm);
}

pub(super) fn report_stylesheet_diagnostics(diagnostics: &[StylesheetResourceDiagnostic]) {
    let mut summaries = BTreeMap::<(String, String, String, String), (usize, &str)>::new();
    for diagnostic in diagnostics {
        let url = diagnostic.requested_url.as_ref().map_or("", Url::as_str);
        let feature = diagnostic
            .message
            .rsplit_once(": ")
            .map_or(diagnostic.message.as_str(), |(_, detail)| detail);
        let key = (
            format!("{:?}", diagnostic.severity),
            format!("{:?}", diagnostic.code),
            url.to_owned(),
            feature.to_owned(),
        );
        let summary = summaries.entry(key).or_insert((0, &diagnostic.message));
        summary.0 = summary.0.saturating_add(1);
    }
    for ((severity, code, url, feature), (count, first_message)) in summaries {
        let occurrences = if count == 1 {
            String::new()
        } else {
            format!(" ({count} occurrences)")
        };
        eprintln!(
            "render-browser stylesheet {severity} {code} {url}: {feature}{occurrences}; first: {first_message}"
        );
    }
}

pub(super) fn report_script_diagnostics(diagnostics: &[ScriptResourceDiagnostic]) {
    for diagnostic in diagnostics {
        let owner = diagnostic
            .owner
            .map_or_else(|| "document".to_owned(), |owner| format!("node {owner:?}"));
        let url = diagnostic
            .requested_url
            .as_ref()
            .map_or("inline", Url::as_str);
        eprintln!(
            "render-browser classic script {:?} {:?} {owner} {url}: {}",
            diagnostic.severity, diagnostic.code, diagnostic.message
        );
    }
}

pub(super) fn report_script_discovery_diagnostics(diagnostics: &[ScriptDiagnostic]) {
    for diagnostic in diagnostics {
        let owner = diagnostic
            .owner
            .map_or_else(|| "document".to_owned(), |owner| format!("node {owner:?}"));
        let source_order = diagnostic
            .source_order
            .map_or_else(|| "unknown".to_owned(), |order| order.to_string());
        eprintln!(
            "render-browser classic script discovery {:?} {owner} source {source_order}: {}",
            diagnostic.code, diagnostic.message
        );
    }
}

pub(super) fn report_image_diagnostics(
    diagnostics: &[render_browser::images::ImageResourceDiagnostic],
) {
    for diagnostic in diagnostics {
        let owner = diagnostic
            .owner
            .map_or_else(|| "document".to_owned(), |owner| format!("node {owner:?}"));
        let url = diagnostic
            .requested_url
            .as_ref()
            .map_or("unknown", Url::as_str);
        eprintln!(
            "render-browser image {:?} {:?} {owner} {url}: {}",
            diagnostic.severity, diagnostic.code, diagnostic.message
        );
    }
}
