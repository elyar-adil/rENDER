//! Headless diagnosis for the saved hao123.com homepage.
//!
//! Usage: `cargo run -p render-core --example hao123_diag -- <saved.html>`
//!
//! Reports stylesheet parse diagnostics, computed-style diagnostics,
//! formatting diagnostics, and layout geometry spot checks so engine gaps
//! (dropped rules, wrong geometry) can be located offline.

use std::collections::BTreeMap;
use std::fs;
use std::process::ExitCode;

use render_core::document::{Document, DocumentRenderOptions};
use render_core::layout::PhysicalSize;

fn main() -> ExitCode {
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(diag_main)
        .expect("spawn diag thread");
    match handle.join() {
        Ok(code) => code,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[allow(clippy::too_many_lines)]
fn diag_main() -> ExitCode {
    let Some(html_path) = std::env::args().nth(1) else {
        eprintln!("usage: hao123_diag <saved.html>");
        return ExitCode::FAILURE;
    };
    let Ok(html) = fs::read_to_string(&html_path) else {
        eprintln!("cannot read {html_path}");
        return ExitCode::FAILURE;
    };

    let document = Document::parse(&html);

    let mut options = DocumentRenderOptions::default();
    options.layout.viewport = PhysicalSize {
        width: 1920.0,
        height: 1080.0,
    };
    let output = document.render_reference(options);

    println!("== document diagnostics (grouped) ==");
    let mut grouped: BTreeMap<String, usize> = BTreeMap::new();
    for diagnostic in &output.diagnostics.document {
        *grouped
            .entry(format!("{:?} {}", diagnostic.code, diagnostic.message))
            .or_default() += 1;
    }
    for (message, count) in &grouped {
        println!("  {count:6} x {message}");
    }
    if grouped.is_empty() {
        println!("  (none)");
    }

    println!("\n== stylesheet parse diagnostics (grouped) ==");
    let mut grouped: BTreeMap<String, usize> = BTreeMap::new();
    for diagnostic in &output.diagnostics.style_sheets {
        *grouped
            .entry(diagnostic.diagnostic.message.clone())
            .or_default() += 1;
    }
    for (message, count) in &grouped {
        println!("  {count:6} x {message}");
    }
    if grouped.is_empty() {
        println!("  (none)");
    }

    println!("\n== computed style diagnostics (grouped) ==");
    let mut grouped: BTreeMap<String, usize> = BTreeMap::new();
    for style in output.styles.values() {
        for diagnostic in style.diagnostics() {
            *grouped.entry(diagnostic.message.clone()).or_default() += 1;
        }
    }
    for (message, count) in &grouped {
        println!("  {count:6} x {message}");
    }
    if grouped.is_empty() {
        println!("  (none)");
    }

    println!("\n== formatting diagnostics (grouped) ==");
    let mut grouped: BTreeMap<String, usize> = BTreeMap::new();
    for diagnostic in output.formatting.diagnostics() {
        *grouped.entry(format!("{:?}", diagnostic.code)).or_default() += 1;
    }
    for (message, count) in &grouped {
        println!("  {count:6} x {message}");
    }
    if grouped.is_empty() {
        println!("  (none)");
    }

    // Geometry spot checks: how many fragments ended up with impossible or
    // suspicious geometry.
    let fragments = &output.layout.fragments;
    let total = fragments.iter().count();
    let mut zero_width = 0usize;
    let mut negative = 0usize;
    let mut beyond_viewport = 0usize;
    let viewport = 1920.0f32;
    for fragment in fragments.iter() {
        let rect = &fragment.rect;
        if rect.size.width < 0.0 || rect.size.height < 0.0 {
            negative += 1;
        }
        if rect.size.width == 0.0 {
            zero_width += 1;
        }
        if rect.origin.x > viewport {
            beyond_viewport += 1;
        }
    }
    println!("\n== layout spot checks ==");
    println!("  fragments total: {total}");
    println!("  negative sizes:  {negative}");
    println!("  zero-width:      {zero_width}");
    println!("  beyond viewport x: {beyond_viewport}");
    println!(
        "  scrollable content: {}x{}",
        fragments.scrollable_content_size.width, fragments.scrollable_content_size.height
    );

    // Dump the first N fragments for manual inspection.
    let count = std::env::var("RENDER_DIAG_FRAGMENTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    if count > 0 {
        println!("\n== first fragments ==");
        for (index, fragment) in fragments.iter().enumerate().take(count) {
            let rect = &fragment.rect;
            println!(
                "  [{index}] node={:?} x={} y={} w={} h={}",
                fragment.formatting_node,
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                rect.size.height
            );
        }
    }

    ExitCode::SUCCESS
}
