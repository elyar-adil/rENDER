//! Official WPT static reftest entry point.
//!
//! The checkout and the case manifest are external inputs. The batch runner
//! discovers official `link rel="match"` pairs and this test renders every
//! pair through render-core's deterministic Document pipeline.

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use render_core::document::{
    AuthorStyleSource, Document, DocumentLimits, DocumentRenderOptions, ExternalStyleSheetKey,
    ExternalStyleSheets,
};
use render_core::dom::{Dom, Node, NodeKind};
use render_core::layout::{LayoutOptions, PhysicalSize};
use render_core::paint::Color;
use url::Url;

const WPT_REVISION: &str = "c7fdee80f3f17b4e9813964916afdfd57ace863f";
const VIEWPORT: PhysicalSize = PhysicalSize {
    width: 800.0,
    height: 600.0,
};

#[derive(Debug)]
enum RenderError {
    Unsupported(String),
    Infrastructure(String),
}

#[derive(Debug)]
struct RenderedPage {
    width: u32,
    height: u32,
    pixels: Vec<Color>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Pass,
    Fail,
    Unsupported,
    Skip,
}

impl Outcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unsupported => "unsupported",
            Self::Skip => "skip",
        }
    }
}

#[derive(Debug)]
struct WptCase {
    test: String,
    reference: String,
}

#[derive(Debug, Default)]
struct Summary {
    cases: usize,
    pass: usize,
    fail: usize,
    unsupported: usize,
    skip: usize,
    infrastructure: usize,
}

impl Summary {
    fn record(&mut self, outcome: Option<Outcome>) {
        self.cases += 1;
        match outcome {
            Some(Outcome::Pass) => self.pass += 1,
            Some(Outcome::Fail) => self.fail += 1,
            Some(Outcome::Unsupported) => self.unsupported += 1,
            Some(Outcome::Skip) => self.skip += 1,
            None => self.infrastructure += 1,
        }
    }
}

#[test]
#[ignore = "requires tools/fetch-wpt.ps1 and an official WPT manifest"]
fn official_wpt_reftests() {
    let root = required_path("RENDER_WPT_ROOT");
    verify_checkout(&root);
    let cases = load_cases();
    assert!(!cases.is_empty(), "official WPT manifest contains no cases");

    let mut summary = Summary::default();
    for case in cases {
        let test_label = case.test.clone();
        let reference_label = case.reference.clone();
        match run_case(&root, &case) {
            Ok((outcome, detail)) => {
                report(outcome, &test_label, &reference_label, &detail);
                summary.record(Some(outcome));
            }
            Err(detail) => {
                report_infrastructure(&test_label, &reference_label, &detail);
                summary.record(None);
            }
        }
    }

    println!(
        "WPT_SUMMARY\tcases={}\tpass={}\tfail={}\tunsupported={}\tskip={}\tinfrastructure={}",
        summary.cases,
        summary.pass,
        summary.fail,
        summary.unsupported,
        summary.skip,
        summary.infrastructure
    );
    assert_eq!(
        summary.infrastructure, 0,
        "official WPT runner infrastructure errors occurred"
    );
    assert_eq!(summary.fail, 0, "official WPT reftest mismatches occurred");
}

fn required_value(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        panic!("{name} is required; set RENDER_WPT_ROOT, RENDER_WPT_TEST, and RENDER_WPT_REFERENCE")
    })
}

fn required_path(name: &str) -> PathBuf {
    PathBuf::from(required_value(name))
}

fn load_cases() -> Vec<WptCase> {
    let test = env::var("RENDER_WPT_TEST").ok();
    let reference = env::var("RENDER_WPT_REFERENCE").ok();
    match (test, reference, env::var_os("RENDER_WPT_MANIFEST")) {
        (Some(test), Some(reference), None) => vec![WptCase { test, reference }],
        (None, None, Some(path)) => read_manifest(&PathBuf::from(path)),
        (Some(_), None, _) | (None, Some(_), _) => {
            panic!("RENDER_WPT_TEST and RENDER_WPT_REFERENCE must be supplied together")
        }
        (Some(_), Some(_), Some(_)) => {
            panic!("use either the single WPT case variables or RENDER_WPT_MANIFEST, not both")
        }
        (None, None, None) => {
            panic!("RENDER_WPT_MANIFEST is required for a batch run; set RENDER_WPT_MANIFEST")
        }
    }
}

fn read_manifest(path: &Path) -> Vec<WptCase> {
    let source = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "cannot read official WPT manifest '{}': {error}",
            path.display()
        )
    });
    let mut cases = Vec::new();
    for (line_number, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let test = fields.next().unwrap_or_default();
        let reference = fields.next().unwrap_or_default();
        assert!(
            !test.is_empty() && !reference.is_empty() && fields.next().is_none(),
            "invalid WPT manifest line {} in '{}': expected TEST<TAB>REFERENCE",
            line_number + 1,
            path.display()
        );
        cases.push(WptCase {
            test: test.to_owned(),
            reference: reference.to_owned(),
        });
    }
    cases
}

fn verify_checkout(root: &Path) {
    let marker = fs::read_to_string(root.join(".render-revision")).unwrap_or_else(|error| {
        panic!(
            "WPT checkout is missing at '{}': run `pwsh -File tools/fetch-wpt.ps1`; marker read failed: {error}",
            root.display()
        )
    });
    assert_eq!(
        marker.trim(),
        WPT_REVISION,
        "WPT marker revision is not pinned to the runner"
    );
    let output = Command::new("git")
        .args([
            "-C",
            root.to_string_lossy().as_ref(),
            "rev-parse",
            "--verify",
            "HEAD",
        ])
        .output()
        .unwrap_or_else(|error| panic!("cannot verify WPT Git checkout: {error}"));
    assert!(
        output.status.success(),
        "WPT root is not a Git checkout; run `pwsh -File tools/fetch-wpt.ps1`"
    );
    let actual = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        actual.trim(),
        WPT_REVISION,
        "WPT checkout revision is not pinned"
    );
}

fn rooted_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute() {
        return Err(format!("WPT path is absolute: {relative}"));
    }
    if relative_path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!("WPT path escapes the checkout: {relative}"));
    }
    let root = fs::canonicalize(root)
        .map_err(|error| format!("cannot canonicalize WPT root '{}': {error}", root.display()))?;
    let path = fs::canonicalize(root.join(relative_path))
        .map_err(|error| format!("cannot resolve WPT file '{relative}': {error}"))?;
    if !path.starts_with(&root) {
        return Err(format!("WPT path escapes the checkout: {relative}"));
    }
    if !path.is_file() {
        return Err(format!("WPT file does not exist: {}", path.display()));
    }
    Ok(path)
}

fn read_source(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| {
        format!(
            "WPT file is not UTF-8 or cannot be read ({}): {error}",
            path.display()
        )
    })
}

fn run_case(root: &Path, case: &WptCase) -> Result<(Outcome, String), String> {
    let test_path = rooted_file(root, &case.test)?;
    let reference_path = rooted_file(root, &case.reference)?;
    let test_source = read_source(&test_path)?;
    let reference_source = read_source(&reference_path)?;
    let test_document = Document::parse(&test_source);
    let reference_document = Document::parse(&reference_source);

    if has_skip_flag(test_document.dom()) || has_skip_flag(reference_document.dom()) {
        return Ok((
            Outcome::Skip,
            "WPT flags require an interactive, print, or paged environment".to_owned(),
        ));
    }
    if has_unsupported_markup(&test_source) || has_unsupported_markup(&reference_source) {
        return Ok((
            Outcome::Unsupported,
            "static render path does not implement scripts, frames, SVG, images, or external resources"
                .to_owned(),
        ));
    }
    if !match_link_targets(test_document.dom(), &test_path, &reference_path) {
        return Ok((
            Outcome::Fail,
            "test does not declare the supplied reference with link rel=match".to_owned(),
        ));
    }

    let actual = match render_file(&test_path) {
        Ok(page) => page,
        Err(RenderError::Unsupported(detail)) => return Ok((Outcome::Unsupported, detail)),
        Err(RenderError::Infrastructure(detail)) => return Err(detail),
    };
    let expected = match render_file(&reference_path) {
        Ok(page) => page,
        Err(RenderError::Unsupported(detail)) => return Ok((Outcome::Unsupported, detail)),
        Err(RenderError::Infrastructure(detail)) => return Err(detail),
    };
    if actual.width != expected.width || actual.height != expected.height {
        return Ok((
            Outcome::Fail,
            format!(
                "surface dimensions differ: actual={}x{}, reference={}x{}",
                actual.width, actual.height, expected.width, expected.height
            ),
        ));
    }

    let mut differing = 0_usize;
    let mut first_difference = None;
    for (index, (actual_pixel, expected_pixel)) in
        actual.pixels.iter().zip(expected.pixels.iter()).enumerate()
    {
        if actual_pixel != expected_pixel {
            differing += 1;
            if first_difference.is_none() {
                let width = usize::try_from(actual.width).expect("viewport width fits usize");
                first_difference =
                    Some((index % width, index / width, *actual_pixel, *expected_pixel));
            }
        }
    }
    if differing != 0 {
        return Ok((
            Outcome::Fail,
            format!("{differing} pixels differ; first difference: {first_difference:?}"),
        ));
    }
    Ok((Outcome::Pass, "exact pixel match".to_owned()))
}

fn render_file(path: &Path) -> Result<RenderedPage, RenderError> {
    let source = read_source(path).map_err(RenderError::Infrastructure)?;
    let document = Document::parse(&source);
    let base_url = Url::from_file_path(path).map_err(|()| {
        RenderError::Infrastructure(format!("cannot create file URL for {}", path.display()))
    })?;
    let discovery = document.discover_author_style_slots(&base_url, DocumentLimits::default());
    let mut external = ExternalStyleSheets::default();
    for slot in discovery.slots {
        let AuthorStyleSource::External { resolved_url, .. } = slot.source else {
            continue;
        };
        let Some(url) = resolved_url else {
            return Err(RenderError::Unsupported(
                "stylesheet URL cannot be resolved".to_owned(),
            ));
        };
        if url.scheme() != "file" {
            return Err(RenderError::Unsupported(format!(
                "network stylesheet is outside the deterministic local runner: {url}"
            )));
        }
        let stylesheet_path = url.to_file_path().map_err(|()| {
            RenderError::Unsupported(format!("stylesheet URL is not a local file: {url}"))
        })?;
        let stylesheet = fs::read_to_string(&stylesheet_path).map_err(|error| {
            RenderError::Infrastructure(format!(
                "cannot read stylesheet {}: {error}",
                stylesheet_path.display()
            ))
        })?;
        let lower = stylesheet.to_ascii_lowercase();
        if lower.contains("url(") || lower.contains("@import") {
            return Err(RenderError::Unsupported(
                "stylesheet image and @import resources are outside the deterministic local runner"
                    .to_owned(),
            ));
        }
        external.insert_css(ExternalStyleSheetKey::new(slot.owner, url), &stylesheet);
    }

    let options = DocumentRenderOptions {
        layout: LayoutOptions {
            viewport: VIEWPORT,
            ..LayoutOptions::default()
        },
        ..DocumentRenderOptions::default()
    };
    let output =
        document.render_reference_with_external_style_sheets(options, &base_url, &external);
    if !document.html_errors().is_empty()
        || !output.diagnostics.document.is_empty()
        || !output.diagnostics.style_sheets.is_empty()
        || !output.diagnostics.computed_styles.is_empty()
        || !output.diagnostics.formatting.is_empty()
        || !output.diagnostics.layout.is_empty()
        || !output.diagnostics.display_list.is_empty()
        || !output.diagnostics.raster.is_empty()
    {
        return Err(RenderError::Unsupported(format!(
            "render-core reported conformance diagnostics: {:?}",
            output.diagnostics
        )));
    }
    Ok(RenderedPage {
        width: output.raster.surface.width(),
        height: output.raster.surface.height(),
        pixels: output.raster.surface.pixels().to_vec(),
    })
}

fn has_unsupported_markup(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    [
        "<script",
        "<iframe",
        "<canvas",
        "<video",
        "<audio",
        "<object",
        "<embed",
        "<svg",
        "<img",
        "url(",
        "@font-face",
    ]
    .into_iter()
    .any(|marker| lower.contains(marker))
}

fn has_skip_flag(dom: &Dom) -> bool {
    let mut pending = vec![dom.document()];
    while let Some(node) = pending.pop() {
        if let Some(NodeKind::Element(element)) = dom.node(node).map(Node::kind) {
            if element.local_name == "meta"
                && dom
                    .attribute(node, "name")
                    .ok()
                    .flatten()
                    .is_some_and(|name| name.eq_ignore_ascii_case("flags"))
                && dom
                    .attribute(node, "content")
                    .ok()
                    .flatten()
                    .is_some_and(|content| {
                        content.split_ascii_whitespace().any(|flag| {
                            matches!(
                                flag.to_ascii_lowercase().as_str(),
                                "interact" | "manual" | "print" | "paged"
                            )
                        })
                    })
            {
                return true;
            }
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().copied());
    }
    false
}

fn match_link_targets(dom: &Dom, test_path: &Path, reference_path: &Path) -> bool {
    let Ok(expected) = reference_path.canonicalize() else {
        return false;
    };
    let mut pending = vec![dom.document()];
    while let Some(node) = pending.pop() {
        if let Some(NodeKind::Element(element)) = dom.node(node).map(Node::kind) {
            if element.local_name == "link"
                && dom
                    .attribute(node, "rel")
                    .ok()
                    .flatten()
                    .is_some_and(|rel| {
                        rel.split_ascii_whitespace()
                            .any(|token| token.eq_ignore_ascii_case("match"))
                    })
                && let Some(href) = dom.attribute(node, "href").ok().flatten()
                && let Ok(base) = Url::from_file_path(test_path)
                && let Ok(target) = base.join(href)
                && target.scheme() == "file"
                && target.query().is_none()
                && target.fragment().is_none()
                && let Ok(path) = target.to_file_path()
                && path.canonicalize().ok().as_ref() == Some(&expected)
            {
                return true;
            }
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().copied());
    }
    false
}

fn report(outcome: Outcome, test: &str, reference: &str, detail: &str) {
    println!(
        "WPT_RESULT\t{}\t{}\t{}\t{}",
        outcome.label(),
        test,
        reference,
        clean_detail(detail)
    );
}

fn report_infrastructure(test: &str, reference: &str, detail: &str) {
    println!(
        "WPT_RESULT\tinfrastructure\t{}\t{}\t{}",
        test,
        reference,
        clean_detail(detail)
    );
}

fn clean_detail(detail: &str) -> String {
    detail.replace(['\t', '\r', '\n'], " ")
}
