//! Offscreen page runner. Opens no native surface and has no window loop.
//!
//! ```text
//! cargo run --release -p render-browser --bin render-headless -- \
//!   --url https://www.example.com/ --out /tmp/page.png --width 1180 --height 780 --dpr 2
//! ```

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use render_browser::headless::{HeadlessOptions, run_headless, write_surface_png};
use render_net::Url;

const DEFAULT_WIDTH: u32 = 1_180;
const DEFAULT_HEIGHT: u32 = 780;
const DEFAULT_DPR: f32 = 1.0;
const DEFAULT_WAIT_MS: u64 = 10_000;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("render-headless: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let Some(cli) = parse_arguments(env::args().skip(1))? else {
        print_usage();
        return Ok(());
    };
    // The JS parser and layout solver recurse deeply on large real pages.
    // Keep the same generous stack the native shell gives its event loop,
    // without borrowing any of its event-loop machinery.
    let worker = std::thread::Builder::new()
        .name("render-headless-page".to_owned())
        .stack_size(512 * 1024 * 1024)
        .spawn(move || execute(cli))
        .map_err(|error| format!("could not start the page thread: {error}"))?;
    worker
        .join()
        .map_err(|_| "the page thread panicked".to_owned())?
}

fn execute(cli: Cli) -> Result<(), String> {
    let report = run_headless(cli.options)?;
    write_surface_png(&report.surface, &cli.out)
        .map_err(|error| format!("could not write {}: {error}", cli.out.display()))?;
    if let Some(path) = &cli.dump_dom {
        let html = report
            .dom_html
            .as_deref()
            .ok_or_else(|| "DOM serialization was not requested".to_owned())?;
        fs::write(path, html)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        eprintln!("render-headless: DOM written to {}", path.display());
    }
    eprintln!(
        "render-headless: wrote {} ({}x{}, dpr {}, ready {}, {} ms{})",
        cli.out.display(),
        report.health.surface_width,
        report.health.surface_height,
        report.health.device_scale,
        report.health.ready_state,
        report.health.elapsed_ms,
        if report.health.timed_out {
            ", timed out"
        } else {
            ""
        },
    );
    if cli.print_health {
        println!("{}", report.health_json());
    }
    Ok(())
}

struct Cli {
    options: HeadlessOptions,
    out: PathBuf,
    dump_dom: Option<PathBuf>,
    print_health: bool,
}

fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<Option<Cli>, String> {
    let mut url = None;
    let mut width = DEFAULT_WIDTH;
    let mut height = DEFAULT_HEIGHT;
    let mut dpr = DEFAULT_DPR;
    let mut wait = Duration::from_millis(DEFAULT_WAIT_MS);
    let mut proxy = None;
    let mut out = None;
    let mut dump_dom = None;
    let mut print_health = false;
    let mut positional = None;
    let mut arguments = arguments;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(None),
            "--url" => url = Some(next_value(&mut arguments, "--url")?),
            "--width" => {
                width = parse_u32(&next_value(&mut arguments, "--width")?, "--width")?;
            }
            "--height" => {
                height = parse_u32(&next_value(&mut arguments, "--height")?, "--height")?;
            }
            "--dpr" => {
                dpr = parse_positive_f32(&next_value(&mut arguments, "--dpr")?, "--dpr")?;
            }
            "--wait-ms" => {
                let millis = next_value(&mut arguments, "--wait-ms")?
                    .parse::<u64>()
                    .map_err(|error| format!("--wait-ms expected milliseconds: {error}"))?;
                wait = Duration::from_millis(millis);
            }
            "--proxy" => proxy = Some(next_value(&mut arguments, "--proxy")?),
            "--out" => out = Some(PathBuf::from(next_value(&mut arguments, "--out")?)),
            "--dump-dom" => {
                dump_dom = Some(PathBuf::from(next_value(&mut arguments, "--dump-dom")?));
            }
            "--print-health" => print_health = true,
            _ if argument.starts_with('-') => {
                return Err(format!(
                    "unknown argument {argument:?}; pass --help for usage"
                ));
            }
            _ => {
                if positional.is_some() {
                    return Err("expected at most one URL or local HTML path".to_owned());
                }
                positional = Some(argument);
            }
        }
    }

    let Some(url) = url.or(positional) else {
        return Err("--url is required; pass --help for usage".to_owned());
    };
    if width == 0 || height == 0 {
        return Err("--width and --height must be non-zero".to_owned());
    }
    let url = parse_page_url(&url)?;
    let out = out.ok_or_else(|| "--out is required; pass --help for usage".to_owned())?;
    Ok(Some(Cli {
        options: HeadlessOptions {
            url,
            viewport_width: width,
            viewport_height: height,
            device_scale: dpr,
            wait,
            proxy,
            dump_dom: dump_dom.is_some(),
        },
        out,
        dump_dom,
        print_health,
    }))
}

fn next_value(arguments: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_u32(value: &str, flag: &str) -> Result<u32, String> {
    let parsed = value
        .parse::<u32>()
        .map_err(|error| format!("{flag} expected a whole number: {error}"))?;
    if parsed == 0 {
        return Err(format!("{flag} must be non-zero"));
    }
    Ok(parsed)
}

fn parse_positive_f32(value: &str, flag: &str) -> Result<f32, String> {
    let parsed = value
        .parse::<f32>()
        .map_err(|error| format!("{flag} expected a number: {error}"))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(format!("{flag} must be a positive finite number"));
    }
    Ok(parsed)
}

fn parse_page_url(value: &str) -> Result<Url, String> {
    if let Ok(url) = Url::parse(value)
        && matches!(url.scheme(), "http" | "https" | "file")
    {
        return Ok(url);
    }
    let path = PathBuf::from(value);
    if path.is_file() {
        let absolute = fs::canonicalize(&path)
            .map_err(|error| format!("could not resolve {}: {error}", path.display()))?;
        return Url::from_file_path(&absolute).map_err(|()| {
            format!(
                "path cannot be represented as a URL: {}",
                absolute.display()
            )
        });
    }
    Err(format!(
        "expected an http, https or file URL, or an existing local HTML file; got {value:?}"
    ))
}

fn print_usage() {
    println!(
        "Usage: render-headless --url URL --out PATH [--width N] [--height N] [--dpr N] \
[--wait-ms N] [--proxy ADDR] [--dump-dom PATH] [--print-health]\n\n\
Loads an http, https or file page without opening any window or graphics surface,\n\
runs its scripts and external resources, and writes an offscreen PNG at width*dpr by height*dpr.\n\
\n\
  --url URL          page to load (http, https, file, or a local .html path)\n\
  --out PATH         PNG output path\n\
  --width N          CSS viewport width (default {DEFAULT_WIDTH})\n\
  --height N         CSS viewport height (default {DEFAULT_HEIGHT})\n\
  --dpr N            device pixel ratio for the raster (default {DEFAULT_DPR})\n\
  --wait-ms N        maximum settle time in milliseconds (default {DEFAULT_WAIT_MS})\n\
  --proxy ADDR       explicit HTTP proxy, or 'direct'\n\
  --dump-dom PATH    write the final serialized DOM\n\
  --print-health     print one JSON object with counters and element geometry\n\
\n\
No window is created. Output is entirely offscreen."
    );
}
