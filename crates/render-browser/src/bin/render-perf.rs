//! Deterministic, headless renderer benchmark.
//!
//! This executable intentionally measures the stable HTML-to-pixels boundary:
//! parsing plus the reference style/layout/paint pipeline. It opens no window,
//! performs no network I/O, and does not execute page scripts, so its output
//! remains useful while those browser-facing layers continue to evolve.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use render_core::document::{Document, DocumentRenderOptions, DocumentRenderOutput};
use render_core::layout::{LayoutOptions, PhysicalPoint, PhysicalSize};

const DEFAULT_ITERATIONS: usize = 5;
const DEFAULT_SCROLL_STEPS: usize = 12;
const DEFAULT_WARMUP_ITERATIONS: usize = 1;
const MAX_ITERATIONS: usize = 10_000;
const MAX_SCROLL_STEPS: usize = 65_535;

const INDEX_FIXTURE: &str = include_str!("../../../../example/index.html");
const HN_FIXTURE: &str = include_str!("../../../../example/hn.html");
const HAO123_FIXTURE: &str = include_str!("../../../../example/hao123.html");

#[derive(Clone, Debug)]
struct Options {
    fixture: String,
    iterations: usize,
    warmup_iterations: usize,
    scroll_steps: usize,
    viewport: PhysicalSize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fixture: "generated".to_owned(),
            iterations: DEFAULT_ITERATIONS,
            warmup_iterations: DEFAULT_WARMUP_ITERATIONS,
            scroll_steps: DEFAULT_SCROLL_STEPS,
            viewport: PhysicalSize {
                width: 1_280.0,
                height: 720.0,
            },
        }
    }
}

#[derive(Debug)]
struct Fixture {
    name: String,
    source: String,
}

#[derive(Clone, Copy, Debug)]
struct OutputShape {
    fragments: usize,
    display_items: usize,
    surface_width: u32,
    surface_height: u32,
    diagnostics: usize,
    html_errors: usize,
}

#[derive(Debug)]
struct FixtureResult {
    fixture: String,
    source_bytes: usize,
    parse_micros: Vec<u128>,
    first_render_micros: Vec<u128>,
    first_visible_micros: Vec<u128>,
    scroll_render_micros: Vec<u128>,
    shape: OutputShape,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("render-perf: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let Some(options) = parse_options(env::args().skip(1))? else {
        print_usage();
        return Ok(());
    };
    let fixtures = load_fixtures(&options.fixture)?;
    let mut results = Vec::with_capacity(fixtures.len());

    for fixture in fixtures {
        for _ in 0..options.warmup_iterations {
            let _ = run_fixture(&fixture, &options);
        }
        results.push(run_fixture(&fixture, &options));
    }

    print_results(&options, &results);
    Ok(())
}

fn parse_options(arguments: impl Iterator<Item = String>) -> Result<Option<Options>, String> {
    let mut options = Options::default();
    let mut arguments = arguments.peekable();

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--help" | "-h" => return Ok(None),
            "--fixture" => options.fixture = next_argument(&mut arguments, "--fixture")?,
            "--iterations" => {
                options.iterations = parse_bounded_usize(
                    &next_argument(&mut arguments, "--iterations")?,
                    "--iterations",
                    1,
                    MAX_ITERATIONS,
                )?;
            }
            "--warmup" => {
                options.warmup_iterations = parse_bounded_usize(
                    &next_argument(&mut arguments, "--warmup")?,
                    "--warmup",
                    0,
                    MAX_ITERATIONS,
                )?;
            }
            "--scroll-steps" => {
                options.scroll_steps = parse_bounded_usize(
                    &next_argument(&mut arguments, "--scroll-steps")?,
                    "--scroll-steps",
                    1,
                    MAX_SCROLL_STEPS,
                )?;
            }
            "--width" => {
                options.viewport.width =
                    parse_dimension(&next_argument(&mut arguments, "--width")?, "--width")?;
            }
            "--height" => {
                options.viewport.height =
                    parse_dimension(&next_argument(&mut arguments, "--height")?, "--height")?;
            }
            _ => {
                return Err(format!(
                    "unknown argument {argument:?}; pass --help for usage"
                ));
            }
        }
    }

    Ok(Some(options))
}

fn next_argument(
    arguments: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    flag: &str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_bounded_usize(
    value: &str,
    flag: &str,
    minimum: usize,
    maximum: usize,
) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|error| format!("{flag} must be an integer: {error}"))?;
    if !(minimum..=maximum).contains(&parsed) {
        return Err(format!("{flag} must be between {minimum} and {maximum}"));
    }
    Ok(parsed)
}

fn parse_dimension(value: &str, flag: &str) -> Result<f32, String> {
    let parsed = value
        .parse::<f32>()
        .map_err(|error| format!("{flag} must be a finite positive number: {error}"))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(format!("{flag} must be a finite positive number"));
    }
    Ok(parsed)
}

fn load_fixtures(request: &str) -> Result<Vec<Fixture>, String> {
    match request {
        "generated" => Ok(vec![Fixture {
            name: "generated".to_owned(),
            source: generated_fixture(),
        }]),
        "index" => Ok(vec![builtin_fixture("index", INDEX_FIXTURE)]),
        "hn" => Ok(vec![builtin_fixture("hn", HN_FIXTURE)]),
        "hao123" => Ok(vec![builtin_fixture("hao123", HAO123_FIXTURE)]),
        "all" => Ok(vec![
            Fixture {
                name: "generated".to_owned(),
                source: generated_fixture(),
            },
            builtin_fixture("index", INDEX_FIXTURE),
            builtin_fixture("hn", HN_FIXTURE),
            builtin_fixture("hao123", HAO123_FIXTURE),
        ]),
        path => {
            let path = PathBuf::from(path);
            let source = fs::read_to_string(&path)
                .map_err(|error| format!("cannot read fixture {}: {error}", path.display()))?;
            Ok(vec![Fixture {
                name: path.display().to_string(),
                source,
            }])
        }
    }
}

fn builtin_fixture(name: &str, source: &str) -> Fixture {
    Fixture {
        name: name.to_owned(),
        source: source.to_owned(),
    }
}

fn generated_fixture() -> String {
    const BACKGROUNDS: [&str; 4] = ["#eef4ff", "#f8f1ff", "#effaf4", "#fff7ed"];
    const ACCENTS: [&str; 4] = ["#2463eb", "#8b39d7", "#16875b", "#c2540a"];

    let mut source = String::from(
        r"<!doctype html><html><head><style>
html, body { display: block; margin: 0; background-color: #f5f7fb; color: #20242c; }
main { display: block; width: 100%; padding-top: 12px; padding-bottom: 12px; }
.card { display: block; width: 1184px; height: 76px; margin-top: 8px; margin-right: 16px; margin-bottom: 8px; margin-left: 16px; padding-top: 10px; padding-right: 14px; padding-bottom: 10px; padding-left: 14px; border-top-width: 1px; border-right-width: 1px; border-bottom-width: 1px; border-left-width: 1px; border-top-style: solid; border-right-style: solid; border-bottom-style: solid; border-left-style: solid; border-top-color: #dce1e8; border-right-color: #dce1e8; border-bottom-color: #dce1e8; border-left-color: #dce1e8; }
.title { display: block; font-size: 17px; font-weight: 700; line-height: 24px; }
.body { display: block; margin-top: 6px; font-size: 13px; line-height: 18px; color: #596273; }
</style></head><body><main>",
    );

    for index in 0..768 {
        let palette_index = index % BACKGROUNDS.len();
        let _ = write!(
            source,
            "<article class=\"card\" style=\"background-color:{}\"><span class=\"title\" style=\"color:{}\">Deterministic render card {index}</span><span class=\"body\">Stable paint workload with text, borders, and a scrollable document.</span></article>",
            BACKGROUNDS[palette_index], ACCENTS[palette_index],
        );
    }
    source.push_str("</main></body></html>");
    source
}

fn run_fixture(fixture: &Fixture, options: &Options) -> FixtureResult {
    let render_options = DocumentRenderOptions {
        layout: LayoutOptions {
            viewport: options.viewport,
            ..LayoutOptions::default()
        },
        ..DocumentRenderOptions::default()
    };
    let mut parse_micros = Vec::with_capacity(options.iterations);
    let mut first_render_micros = Vec::with_capacity(options.iterations);
    let mut first_visible_micros = Vec::with_capacity(options.iterations);
    let mut scroll_render_micros =
        Vec::with_capacity(options.iterations.saturating_mul(options.scroll_steps));
    let mut shape = OutputShape {
        fragments: 0,
        display_items: 0,
        surface_width: 0,
        surface_height: 0,
        diagnostics: 0,
        html_errors: 0,
    };

    for _ in 0..options.iterations {
        let first_visible_started = Instant::now();
        let parse_started = Instant::now();
        let document = Document::parse(&fixture.source);
        parse_micros.push(parse_started.elapsed().as_micros());

        let render_started = Instant::now();
        let mut output = document.render_reference(render_options);
        first_render_micros.push(render_started.elapsed().as_micros());
        first_visible_micros.push(first_visible_started.elapsed().as_micros());

        let maximum_scroll = output.layout.fragments.max_scroll_offset().y;
        for step in 0..options.scroll_steps {
            let fraction = scroll_fraction(step, options.scroll_steps);
            let scroll_started = Instant::now();
            output = document.render_reference(DocumentRenderOptions {
                scroll_offset: PhysicalPoint {
                    x: 0.0,
                    y: maximum_scroll * fraction,
                },
                ..render_options
            });
            scroll_render_micros.push(scroll_started.elapsed().as_micros());
        }
        shape = output_shape(&document, &output);
    }

    FixtureResult {
        fixture: fixture.name.clone(),
        source_bytes: fixture.source.len(),
        parse_micros,
        first_render_micros,
        first_visible_micros,
        scroll_render_micros,
        shape,
    }
}

fn scroll_fraction(step: usize, total: usize) -> f32 {
    let numerator =
        u16::try_from(step.saturating_add(1)).expect("scroll step is bounded by MAX_SCROLL_STEPS");
    let denominator = u16::try_from(total).expect("scroll steps are bounded by MAX_SCROLL_STEPS");
    f32::from(numerator) / f32::from(denominator)
}

fn output_shape(document: &Document, output: &DocumentRenderOutput) -> OutputShape {
    let diagnostics = output.diagnostics.document.len()
        + output.diagnostics.style_sheets.len()
        + output.diagnostics.computed_styles.len()
        + output.diagnostics.formatting.len()
        + output.diagnostics.layout.len()
        + output.diagnostics.display_list.len()
        + output.diagnostics.raster.len();
    OutputShape {
        fragments: output.layout.fragments.iter().count(),
        display_items: output.display.list.items().len(),
        surface_width: output.raster.surface.width(),
        surface_height: output.raster.surface.height(),
        diagnostics,
        html_errors: document.html_errors().len(),
    }
}

fn print_results(options: &Options, results: &[FixtureResult]) {
    print!(
        "{{\"schema_version\":1,\"iterations\":{},\"warmup_iterations\":{},\"scroll_steps\":{},\"viewport\":{{\"width\":{},\"height\":{}}},\"results\":[",
        options.iterations,
        options.warmup_iterations,
        options.scroll_steps,
        options.viewport.width,
        options.viewport.height,
    );
    for (index, result) in results.iter().enumerate() {
        if index > 0 {
            print!(",");
        }
        print_result(result);
    }
    println!("]}}");
}

fn print_result(result: &FixtureResult) {
    print!(
        "{{\"fixture\":\"{}\",\"source_bytes\":{},\"parse\":",
        json_escape(&result.fixture),
        result.source_bytes,
    );
    print_distribution(&result.parse_micros);
    print!(",\"first_render\":");
    print_distribution(&result.first_render_micros);
    print!(",\"first_visible\":");
    print_distribution(&result.first_visible_micros);
    print!(",\"scroll_render\":");
    print_distribution(&result.scroll_render_micros);
    println!(
        ",\"output\":{{\"fragments\":{},\"display_items\":{},\"surface\":{{\"width\":{},\"height\":{}}},\"diagnostics\":{},\"html_errors\":{}}}}}",
        result.shape.fragments,
        result.shape.display_items,
        result.shape.surface_width,
        result.shape.surface_height,
        result.shape.diagnostics,
        result.shape.html_errors,
    );
}

fn print_distribution(values: &[u128]) {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let count = sorted.len();
    let median = if count & 1 == 0 {
        sorted[count / 2 - 1].saturating_add(sorted[count / 2]) / 2
    } else {
        sorted[count / 2]
    };
    let p95_index = count.saturating_mul(95).saturating_add(99) / 100 - 1;
    print!(
        "{{\"unit\":\"us\",\"samples\":{},\"min\":{},\"median\":{},\"p95\":{},\"max\":{}}}",
        count,
        sorted[0],
        median,
        sorted[p95_index],
        sorted[count - 1],
    );
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(escaped, "\\u{:04x}", u32::from(character));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn print_usage() {
    println!(
        "Usage: cargo run --release -p render-browser --bin render-perf -- [OPTIONS]\n\n\
         Options:\n\
           --fixture <generated|index|hn|hao123|all|PATH>  Fixture to render (default: generated)\n\
           --iterations <N>                                Measured samples, 1..={MAX_ITERATIONS} (default: {DEFAULT_ITERATIONS})\n\
           --warmup <N>                                    Unreported warmup samples (default: {DEFAULT_WARMUP_ITERATIONS})\n\
           --scroll-steps <N>                              Renders per measured scroll, 1..={MAX_SCROLL_STEPS} (default: {DEFAULT_SCROLL_STEPS})\n\
           --width <PX> --height <PX>                      CSS viewport (default: 1280 x 720)\n\
           -h, --help                                      Show this help\n\n\
         The successful output is one JSON document on stdout."
    );
}

#[cfg(test)]
mod tests {
    use super::{json_escape, parse_options, scroll_fraction};

    #[test]
    fn parses_explicit_benchmark_options() {
        let options = parse_options(
            [
                "--fixture",
                "hn",
                "--iterations",
                "3",
                "--warmup",
                "0",
                "--scroll-steps",
                "4",
                "--width",
                "800",
                "--height",
                "600",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("options should parse")
        .expect("help was not requested");

        assert_eq!(options.fixture, "hn");
        assert_eq!(options.iterations, 3);
        assert_eq!(options.warmup_iterations, 0);
        assert_eq!(options.scroll_steps, 4);
        assert!((options.viewport.width - 800.0).abs() < f32::EPSILON);
        assert!((options.viewport.height - 600.0).abs() < f32::EPSILON);
    }

    #[test]
    fn escapes_json_and_spans_the_full_scroll_range() {
        assert_eq!(json_escape("a\"\\\n"), "a\\\"\\\\\\n");
        assert!((scroll_fraction(0, 4) - 0.25).abs() < f32::EPSILON);
        assert!((scroll_fraction(3, 4) - 1.0).abs() < f32::EPSILON);
    }
}
