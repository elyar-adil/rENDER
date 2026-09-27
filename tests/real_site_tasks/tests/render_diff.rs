//! Report-only render-and-diff over every fixture.
//!
//! This target renders each fixture headlessly with the reference backends,
//! compares the raster against a checked-in baseline **when one exists**, and
//! prints the result. It never fails on a pixel difference and it never creates
//! a baseline. Both of those are deliberate:
//!
//! * `docs/visual_fidelity_gaps.md` records that today's renderer is wrong in
//!   several known ways, so a baseline generated from current output would
//!   encode that wrongness as truth.
//! * A pixel gate that fails while those gaps are open trains everyone to ignore
//!   it, which is worse than having no gate.
//!
//! So: no baseline is checked in today, every row reads `no-baseline`, and this
//! test passes unconditionally. Promoting a baseline is a human decision, made
//! through `real-site-shots`; `docs/real_site_acceptance.md` spells the
//! procedure out.
//!
//! Run it with `--nocapture` to see the report:
//!
//! ```text
//! cargo test --manifest-path tests/real_site_tasks/Cargo.toml --test render_diff -- --nocapture
//! ```

use real_site_tasks::fixture::{FIXTURES, baseline_path};
use real_site_tasks::harness::Session;
use real_site_tasks::shots;

#[test]
fn renders_every_fixture_and_reports_the_baseline_difference() {
    let viewport = real_site_tasks::harness::contract_viewport();
    println!(
        "render-and-diff report (viewport {}x{}, reference backends, no system fonts)",
        viewport.width, viewport.height
    );
    println!("report-only: a pixel difference is printed, never asserted\n");

    for fixture in FIXTURES {
        let session = Session::load(fixture);
        let shot = shots::capture(&session);
        let baseline = shots::read_baseline(&baseline_path(fixture.label));
        let comparison = shots::compare(&shot, baseline.as_ref());
        println!("{}", shots::describe(fixture, &comparison));
        println!(
            "    raster {}x{}, digest {}, {} display items, {} fragments, max scroll y {}",
            shot.width,
            shot.height,
            shot.digest(),
            session.display_list().items().len(),
            session.output.layout.fragments.iter().count(),
            session.output.layout.fragments.max_scroll_offset().y,
        );
    }

    println!(
        "\nbaseline directory: {}\n\
         No baseline is checked in. To promote one, look at a rendered PNG and then run\n\
         `real-site-shots --label <fixture> --promote`. See docs/real_site_acceptance.md.",
        real_site_tasks::fixture::baseline_root().display()
    );
}

#[test]
fn the_report_does_not_depend_on_which_baselines_happen_to_exist() {
    // Guards the report-only property itself: the outcome of the comparison is
    // computed and returned, and the test never turns it into a verdict. This
    // test also pins that a fixture with no baseline and the same fixture with
    // its own shot as baseline differ only in the status field.
    let session = Session::load(&real_site_tasks::fixture::BAIDU_HOME);
    let shot = shots::capture(&session);
    let without = shots::compare(&shot, None);
    let with_self = shots::compare(&shot, Some(&shot));
    assert_eq!(without.differing_pixels, 0);
    assert_eq!(with_self.differing_pixels, 0);
    assert_ne!(without.status, with_self.status);
}
