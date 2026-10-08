//! Headless offline replay for a saved script-driven home page.
//!
//! The page shape this tool diagnoses: an HTML shell whose visible content is
//! produced by external bundles. The markup alone has nothing in it, so the
//! only way to know whether the engine can run the page is to run the captured
//! scripts against the captured DOM. That is what this does, and it is
//! deliberately site-neutral — the corpus is whatever directory you point it
//! at, because the capability being measured ("can this engine execute a
//! production bundle, and where does it stop") is a property of the engine,
//! not of any one page.
//!
//! Usage:
//! `cargo run -p render-js --example offline_page_replay -- <corpus-dir> [base-url]`
//!
//! `<corpus-dir>` must contain `page.html` plus an `assets/` directory. The
//! assets directory must contain a `manifest.txt` whose lines map an absolute
//! resource URL to a local file name ("url<TAB>file") so external scripts
//! replay exactly as the browser fetched them. `<base-url>` defaults to
//! `https://example.com/` and sets the document URL used for relative
//! resolution, which matters because bundles read `location`.
//!
//! Execution mirrors the browser: parser-blocking inline/classic scripts run
//! in DOM order, while `defer`, `type=module`, and `async` scripts run after
//! parsing (module and defer in document order). Microtasks drain after each
//! script task.
//!
//! Diagnostics:
//! - `RENDER_DIAG_STACK=1` installs a `TypeError` wrapper before the page
//!   scripts so every thrown `TypeError` logs its JS stack to the console.
//! - `RENDER_JS_FRAME_OFFSETS=1` appends `@<source-offset>` to call frame
//!   labels (`Error.stack`, `debug_call_stack`) for minified-bundle mapping.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use render_dom::{NodeId, NodeKind};
use render_html::parse_document;
use render_js::JsRuntime;
use url::Url;

const DEFAULT_BASE_URL: &str = "https://example.com/";

#[allow(clippy::too_many_lines, reason = "offline replay reads as one listing")]
fn main() -> ExitCode {
    // Minified real-world bundles recurse deeply; mirror the browser shell
    // and run everything on a dedicated big-stack thread.
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(diag_main)
        .expect("spawn diag thread");
    match handle.join() {
        Ok(code) => code,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

struct PendingScript {
    label: String,
    source: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scheduling {
    ParserBlocking,
    Deferred,
}

#[allow(clippy::too_many_lines, reason = "offline replay reads as one listing")]
fn diag_main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let Some(corpus_dir) = arguments.next().map(PathBuf::from) else {
        eprintln!(
            "usage: offline_page_replay <corpus-dir> [base-url]\n\
             <corpus-dir> holds page.html and assets/manifest.txt"
        );
        return ExitCode::FAILURE;
    };
    let base_url = arguments
        .next()
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    let html_path = corpus_dir.join("page.html");
    let assets_dir = corpus_dir.join("assets");

    let manifest = load_manifest(&assets_dir).unwrap_or_default();
    let Ok(html) = fs::read_to_string(&html_path) else {
        eprintln!("cannot read {}", html_path.display());
        return ExitCode::FAILURE;
    };

    let mut parsed = parse_document(&html);
    let dom = &mut parsed.dom;

    let mut blocking = Vec::new();
    let mut deferred = Vec::new();
    collect_scripts(
        dom,
        dom.document(),
        &manifest,
        &assets_dir,
        &base_url,
        &mut blocking,
        &mut deferred,
    );
    println!(
        "document: {} parser-blocking scripts, {} deferred/module scripts, {} manifest entries",
        blocking.len(),
        deferred.len(),
        manifest.len(),
    );

    let base = Url::parse(&base_url).expect("base URL");
    let mut runtime = JsRuntime::with_url(dom, &base);
    // Optional diagnostic prelude: wrap global `TypeError` so every thrown
    // TypeError logs its JS stack before propagating (RENDER_DIAG_STACK=1).
    if std::env::var_os("RENDER_DIAG_STACK").is_some() {
        let prelude = r#"
            (function () {
                var Original = TypeError;
                function TypeErrorPatch(message) {
                    var error = new Original(message);
                    try {
                        console.error("[TypeError] " + message + "\n" + error.stack);
                    } catch (logError) {}
                    return error;
                }
                TypeErrorPatch.prototype = Original.prototype;
                globalThis.TypeError = TypeErrorPatch;
            })();
        "#;
        if let Err(error) = runtime.execute(dom, prelude) {
            println!("prelude failed: {error}");
        }
    }
    let mut failures = 0usize;
    for script in blocking.iter().chain(deferred.iter()) {
        let result = runtime.execute(dom, &script.source);
        for message in runtime.take_console_messages() {
            let text: String = message.text.chars().take(2000).collect();
            println!("[console.{level}] {text}", level = message.level.label());
        }
        if let Err(error) = result {
            failures += 1;
            println!("SCRIPT ERROR in {}: {error}", script.label);
            let mut frames = 0;
            for frame in runtime.debug_call_stack() {
                println!("    at {frame}");
                frames += 1;
                if frames >= 12 {
                    println!("    …");
                    break;
                }
            }
        }
        drain_microtasks(&mut runtime, dom);
    }
    println!("script executions finished: {failures} threw");

    drain_microtasks(&mut runtime, dom);
    let timers = runtime.take_pending_timer_requests();
    let fetches = runtime.take_pending_fetch_requests();
    let navigations = runtime.take_pending_navigations();
    println!(
        "after drain: timers={} fetches={} navigations={}",
        timers.len(),
        fetches.len(),
        navigations.len()
    );
    for fetch in &fetches {
        println!("    pending fetch {} {}", fetch.method, fetch.url);
    }

    // App-shell probe: how far the bundles got. These are the globals and
    // mount points a single-page shell publishes, checked by shape rather than
    // by site, so the same probe works on any captured SPA home page.
    let probe = runtime.execute(
        dom,
        r"[typeof window.__HOME_PAGE_PERFORMANCE__,
            typeof window.__MIRROR_REPORT__,
            typeof window.performance.timing,
            typeof window.performance.timing && performance.timing.navigationStart,
            typeof window.__BOOT_CONFIG__,
            typeof window.__pinia,
            typeof window.__APP__,
            document.querySelector('#app') ? document.querySelector('#app').children.length : -1].join(' | ')",
    );
    match probe {
        Ok(outcome) => println!("probe: {}", outcome.value.to_js_string()),
        Err(error) => println!("probe failed: {error}"),
    }
    if let Ok(outcome) =
        runtime.execute(dom, "document.querySelector('#app').innerHTML.slice(0,600)")
    {
        println!("app markup: {}", outcome.value.to_js_string());
    }
    if let Ok(outcome) = runtime.execute(dom, "(function(){var n=document.querySelector('.recommended-swipe');var a=[];for(var i=0;n&&i<5;i++,n=n.parentElement)a.push(n.tagName+'.'+n.className);return a.join(' > ')})()") {
        println!("recommended chain: {}", outcome.value.to_js_string());
    }
    ExitCode::SUCCESS
}

fn drain_microtasks(runtime: &mut JsRuntime, dom: &mut render_dom::Dom) {
    for _ in 0..256 {
        let microtasks = runtime.take_pending_microtasks();
        if microtasks.is_empty() {
            break;
        }
        for microtask in microtasks {
            let label = format!("{microtask:?}");
            if let Err(error) = runtime.invoke_microtask(dom, microtask) {
                println!("  [microtask error] {error} from {label}");
            }
        }
    }
}

fn load_manifest(dir: &Path) -> Option<HashMap<String, String>> {
    let text = fs::read_to_string(dir.join("manifest.txt")).ok()?;
    let mut map = HashMap::new();
    for line in text.lines() {
        let Some((url, file)) = line.split_once('\t') else {
            continue;
        };
        map.insert(url.trim().to_owned(), file.trim().to_owned());
    }
    Some(map)
}

fn attribute(element: &render_dom::ElementData, name: &str) -> Option<String> {
    element
        .attributes
        .iter()
        .find(|attribute| attribute.local_name == name)
        .map(|attribute| attribute.value.clone())
}

fn has_attribute(element: &render_dom::ElementData, name: &str) -> bool {
    element
        .attributes
        .iter()
        .any(|attribute| attribute.local_name == name)
}

#[allow(
    clippy::too_many_arguments,
    reason = "replay threads the corpus through the walk"
)]
fn collect_scripts(
    dom: &render_dom::Dom,
    node: NodeId,
    manifest: &HashMap<String, String>,
    assets_dir: &Path,
    base_url: &str,
    blocking: &mut Vec<PendingScript>,
    deferred: &mut Vec<PendingScript>,
) {
    let Some(node_ref) = dom.node(node) else {
        return;
    };
    if let NodeKind::Element(element) = node_ref.kind()
        && element.local_name == "script"
    {
        let script_type = attribute(element, "type").unwrap_or_default();
        let executable = script_type.is_empty()
            || script_type.contains("javascript")
            || script_type.contains("ecmascript")
            || script_type == "module";
        let is_module = script_type.eq_ignore_ascii_case("module");
        if executable && !has_attribute(element, "nomodule") {
            let scheduling = if is_module || has_attribute(element, "defer") {
                Scheduling::Deferred
            } else {
                Scheduling::ParserBlocking
            };
            let list = match scheduling {
                Scheduling::ParserBlocking => &mut *blocking,
                Scheduling::Deferred => &mut *deferred,
            };
            if let Some(src) = attribute(element, "src") {
                let url = resolve(&src, base_url);
                match url.as_ref().and_then(|url| manifest.get(url.as_str())) {
                    Some(file) => {
                        let path = assets_dir.join(file);
                        match fs::read_to_string(&path) {
                            Ok(source) => {
                                let label = format!("{src} [{file}]");
                                list.push(PendingScript { label, source });
                            }
                            Err(error) => {
                                println!("cannot read asset {}: {error}", path.display());
                            }
                        }
                    }
                    None => println!("no manifest entry for {src}"),
                }
            } else {
                let text = inline_text(dom, node);
                let label = format!("inline[{}…]", &text[..text.len().min(40)]);
                list.push(PendingScript {
                    label,
                    source: text,
                });
            }
        }
    }
    for child in dom.children(node).unwrap_or_default() {
        collect_scripts(
            dom, *child, manifest, assets_dir, base_url, blocking, deferred,
        );
    }
}

fn inline_text(dom: &render_dom::Dom, node: NodeId) -> String {
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

fn resolve(reference: &str, base_url: &str) -> Option<String> {
    let expanded = if let Some(rest) = reference.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        reference.to_owned()
    };
    Url::options()
        .base_url(Some(&Url::parse(base_url).expect("base")))
        .parse(&expanded)
        .ok()
        .map(|url| url.to_string())
}
