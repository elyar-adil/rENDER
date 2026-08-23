//! Headless diagnosis for a saved real-world page (e.g. baidu.com).
//!
//! Usage: cargo run -p render-core --example `baidu_diag` -- <saved.html> [assets-dir]
//!
//! The optional assets directory must contain a `manifest.txt` whose lines map
//! an absolute resource URL to a local file name ("url<TAB>file"), plus those
//! files, so externally referenced scripts and stylesheets can be replayed
//! offline exactly as the browser would have fetched them.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use render_core::css::stylesheet::parse_stylesheet;
use render_core::dom::{NodeId, NodeKind};
use render_core::html::parse_document;
use render_core::js::JsRuntime;
use url::Url;

const BASE_URL: &str = "http://www.baidu.com/";
const MAX_REPORTED_ERRORS: usize = 40;

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let Some(html_path) = arguments.next() else {
        eprintln!("usage: baidu_diag <saved.html> [assets-dir]");
        return ExitCode::FAILURE;
    };
    let assets_dir = arguments.next().map(PathBuf::from);
    let manifest = assets_dir
        .as_ref()
        .map(|dir| load_manifest(dir).unwrap_or_default())
        .unwrap_or_default();

    let Ok(html) = fs::read_to_string(&html_path) else {
        eprintln!("cannot read {html_path}");
        return ExitCode::FAILURE;
    };

    let mut parsed = parse_document(&html);
    let dom = &mut parsed.dom;

    let mut scripts = Vec::new();
    let mut style_sources = Vec::new();
    collect_resources(
        dom,
        dom.document(),
        &manifest,
        &mut scripts,
        &mut style_sources,
    );

    println!(
        "document: {} scripts ({} inline), {} stylesheet sources",
        scripts.len(),
        scripts.iter().filter(|script| script.inline).count(),
        style_sources.len(),
    );

    run_stylesheets(&style_sources);
    run_scripts(dom, &scripts);

    ExitCode::SUCCESS
}

struct PendingScript {
    label: String,
    inline: bool,
    source: String,
}

fn collect_resources(
    dom: &render_core::dom::Dom,
    node: NodeId,
    manifest: &HashMap<String, String>,
    scripts: &mut Vec<PendingScript>,
    styles: &mut Vec<(String, String)>,
) {
    let Some(node_ref) = dom.node(node) else {
        return;
    };
    if let NodeKind::Element(element) = node_ref.kind() {
        let attribute = |name: &str| {
            element
                .attributes
                .iter()
                .find(|attribute| attribute.local_name == name)
                .map(|attribute| attribute.value.clone())
        };
        match element.local_name.as_str() {
            "script" => {
                // Skip non-executable script types (JSON data blocks, templates).
                let type_attr = attribute("type").unwrap_or_default();
                if !type_attr.is_empty()
                    && !type_attr.contains("javascript")
                    && !type_attr.contains("ecmascript")
                {
                    return;
                }
                if let Some(src) = attribute("src") {
                    let resolved = resolve_against_base(&src);
                    let source = resolved
                        .as_ref()
                        .and_then(|url| manifest.get(url.as_str()))
                        .and_then(|file| fs::read_to_string(file).ok());
                    match (resolved, source) {
                        (Some(url), Some(source)) => scripts.push(PendingScript {
                            label: url.to_string(),
                            inline: false,
                            source,
                        }),
                        (Some(url), None) => println!("MISSING external script: {url}"),
                        (None, _) => println!("unresolvable script src: {src}"),
                    }
                } else {
                    let text = child_text(dom, node);
                    if !text.trim().is_empty() {
                        scripts.push(PendingScript {
                            label: "inline".to_owned(),
                            inline: true,
                            source: text,
                        });
                    }
                }
            }
            "style" => {
                let text = child_text(dom, node);
                if !text.trim().is_empty() {
                    styles.push(("inline <style>".to_owned(), text));
                }
            }
            "link" => {
                let rel = attribute("rel").unwrap_or_default().to_ascii_lowercase();
                if rel.split_whitespace().any(|token| token == "stylesheet")
                    && let Some(href) = attribute("href")
                {
                    let resolved = resolve_against_base(&href);
                    let source = resolved
                        .as_ref()
                        .and_then(|url| manifest.get(url.as_str()))
                        .and_then(|file| fs::read_to_string(file).ok());
                    match (resolved, source) {
                        (Some(url), Some(source)) => {
                            styles.push((url.to_string(), source));
                        }
                        (Some(url), None) => println!("MISSING stylesheet: {url}"),
                        (None, _) => println!("unresolvable stylesheet href: {href}"),
                    }
                }
            }
            _ => {}
        }
    }
    for child in dom.children(node).unwrap_or_default().to_vec() {
        collect_resources(dom, child, manifest, scripts, styles);
    }
}

fn child_text(dom: &render_core::dom::Dom, node: NodeId) -> String {
    let mut text = String::new();
    for child in dom.children(node).unwrap_or_default() {
        if let Some(child_ref) = dom.node(*child)
            && let NodeKind::Text(data) = child_ref.kind()
        {
            text.push_str(data);
        }
    }
    text
}

fn resolve_against_base(reference: &str) -> Option<Url> {
    let expanded = if let Some(rest) = reference.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        reference.to_owned()
    };
    Url::options()
        .base_url(Some(&Url::parse(BASE_URL).ok()?))
        .parse(&expanded)
        .ok()
}

fn run_scripts(dom: &mut render_core::dom::Dom, scripts: &[PendingScript]) {
    let base = Url::parse(BASE_URL).expect("base URL");
    let mut runtime = JsRuntime::with_url(dom, &base);
    let mut errors = 0;
    for script in scripts {
        let result = runtime.execute(dom, &script.source);
        for message in runtime.take_console_messages() {
            println!("[console.{}] {}", message.level.label(), message.text);
        }
        match result {
            Ok(_) => {}
            Err(error) => {
                errors += 1;
                // Dump failing inline scripts for offline probing.
                if script.inline && std::env::var("RENDER_DUMP_INLINE").is_ok() {
                    let dump_dir = std::env::temp_dir().join("opencode\\inline_scripts");
                    let _ = std::fs::create_dir_all(&dump_dir);
                    let file_name = format!("inline_{errors:03}.js");
                    let _ = std::fs::write(dump_dir.join(&file_name), &script.source);
                    println!("  dumped → {}\\{}", dump_dir.display(), file_name);
                }
                if errors <= MAX_REPORTED_ERRORS {
                    let message = error.message();
                    let message = if message.len() > 240 {
                        format!("{}…", &message[..240])
                    } else {
                        message.to_owned()
                    };
                    println!("SCRIPT ERROR in {}: {message}", script.label);
                }
            }
        }
    }
    if errors > MAX_REPORTED_ERRORS {
        println!("… and {} more script errors", errors - MAX_REPORTED_ERRORS);
    }
    println!("script executions finished: {errors} threw");

    let probe = runtime
        .execute(
            dom,
            r#"
                var names = ["XMLHttpRequest", "fetch", "localStorage", "sessionStorage",
                    "getComputedStyle", "matchMedia", "MutationObserver", "IntersectionObserver",
                    "ResizeObserver", "history", "JSON", "Promise", "Symbol", "Proxy", "Reflect",
                    "customElements", "navigator.clipboard", "performance"];
                var missing = [];
                var check = function () {
                    for (var index = 0; index < names.length; index += 1) {
                        if (typeof this[names[index]] === "undefined") {
                            missing.push(names[index]);
                        }
                    }
                    return missing.join(",");
                };
                check();
            "#,
        )
        .map_or_else(
            |error| format!("probe failed: {error}"),
            |outcome| match outcome.value {
                render_core::js::JsValue::String(missing) => missing,
                other => format!("probe produced {other:?}"),
            },
        );
    println!("missing globals: {probe}");
}

fn run_stylesheets(sources: &[(String, String)]) {
    let mut total_diagnostics = 0usize;
    for (label, source) in sources {
        let sheet = parse_stylesheet(source);
        total_diagnostics += sheet.diagnostics.len();
        if !sheet.diagnostics.is_empty() {
            println!(
                "{}: {} CSS diagnostics (first few:)",
                label,
                sheet.diagnostics.len()
            );
            for diagnostic in sheet.diagnostics.iter().take(5) {
                println!("  - {}", diagnostic.message);
            }
        }
    }
    println!("CSS diagnostics total: {total_diagnostics}");
}

fn load_manifest(dir: &std::path::Path) -> Result<HashMap<String, String>, std::io::Error> {
    let manifest_path = dir.join("manifest.txt");
    let content = fs::read_to_string(manifest_path)?;
    let mut map = HashMap::new();
    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, '\t');
        if let (Some(url), Some(file)) = (parts.next(), parts.next()) {
            map.insert(
                url.trim().to_owned(),
                dir.join(file).to_string_lossy().into_owned(),
            );
        }
    }
    Ok(map)
}
