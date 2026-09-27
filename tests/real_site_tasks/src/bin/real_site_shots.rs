//! Render a fixture and hand a human a PNG to look at.
//!
//! This is the human side of the report-only render-and-diff capability. It
//! renders a fixture through the engine's reference backends (never system
//! fonts, so the bytes are reproducible) and writes a PNG.
//!
//! Two modes, and the difference matters:
//!
//! * `--out <path>` writes a *review shot* to a path you choose. Look at it.
//!   Decide whether the page is right.
//! * `--promote` writes a *baseline* into
//!   `tests/fixtures/real_sites/baselines/<label>.png`. That is the step that
//!   makes a fixture diff-able from then on, and it is deliberately a separate,
//!   explicit command: nobody should be able to promote a baseline by running
//!   the tests.
//!
//! `docs/real_site_acceptance.md` describes the full promotion procedure.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use real_site_tasks::fixture::{FIXTURES, baseline_path, by_label};
use real_site_tasks::harness::{Session, contract_viewport};
use real_site_tasks::shots::{BaselineStatus, capture, compare, encode_png, read_baseline};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("real-site-shots: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let mut label: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut promote = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--label" => {
                label = Some(
                    arguments
                        .next()
                        .ok_or_else(|| "--label requires a value".to_owned())?,
                );
            }
            "--out" => {
                out = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| "--out requires a path".to_owned())?,
                ));
            }
            "--promote" => promote = true,
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            other => return Err(format!("unknown argument {other:?}; pass --help")),
        }
    }

    let Some(label) = label else {
        print_usage();
        return Ok(());
    };
    let fixture = by_label(&label).ok_or_else(|| {
        let known: Vec<&str> = FIXTURES.iter().map(|fixture| fixture.label).collect();
        format!(
            "no fixture named {label:?}; known labels: {}",
            known.join(", ")
        )
    })?;

    let session = Session::load(fixture);
    let shot = capture(&session);
    let viewport = contract_viewport();
    println!(
        "{label}: rendered {}x{} (viewport {}x{}) from {} fixture bytes",
        shot.width, shot.height, viewport.width, viewport.height, fixture.html_file
    );
    println!("  raster digest {}", shot.digest());

    let baseline_path = baseline_path(fixture.label);
    let baseline = read_baseline(&baseline_path);
    let comparison = compare(&shot, baseline.as_ref());
    println!(
        "  {}",
        real_site_tasks::shots::describe(fixture, &comparison)
    );

    if let Some(path) = &out {
        write_png(path, &shot)?;
        println!("  review shot written to {}", path.display());
    }

    if promote {
        if !path_is_reviewed(out.as_ref()) {
            return Err(
                "--promote requires that you have looked at the review shot first: pass \
                 --out <path>, open it, and only then re-run with --promote"
                    .to_owned(),
            );
        }
        if let Some(parent) = baseline_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        fs::write(&baseline_path, encode_png(&shot))
            .map_err(|error| format!("cannot write {}: {error}", baseline_path.display()))?;
        println!("  baseline promoted to {}", baseline_path.display());
        println!("  Remember to look at the promoted image before committing it.");
    } else if comparison.status == BaselineStatus::Absent {
        println!(
            "  no baseline for this fixture; `real-site-shots --label {label} --out <path>` \
             produces one to review"
        );
    }

    Ok(())
}

/// A promotion is only allowed alongside an explicit review-shot destination,
/// so the command that writes a baseline always names the image that was
/// looked at.
fn path_is_reviewed(out: Option<&PathBuf>) -> bool {
    out.is_some_and(|path| path.is_file())
}

fn write_png(path: &PathBuf, shot: &real_site_tasks::shots::Shot) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    fs::write(path, encode_png(shot))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn print_usage() {
    println!(
        "Usage: cargo run --manifest-path tests/real_site_tasks/Cargo.toml --bin real-site-shots -- [OPTIONS]\n\n\
         Renders a real-site fixture headlessly with the reference backends and writes a PNG\n\
         for a human to look at. Deterministic: no system fonts are involved.\n\n\
         Options:\n\
         \x20 --label <name>   Fixture to render; one of: {labels}\n\
         \x20 --out <path>     Write a review shot for a human to open\n\
         \x20 --promote        Write tests/fixtures/real_sites/baselines/<name>.png\n\
         \x20 -h, --help       Show this help\n\n\
         A baseline is a human decision. --promote is refused unless --out already\n\
         points at a review shot, so running the tests can never create one.",
        labels = FIXTURES
            .iter()
            .map(|fixture| fixture.label)
            .collect::<Vec<_>>()
            .join(", ")
    );
}
