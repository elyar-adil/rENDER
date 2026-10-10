//! Real-page captures: reduced snapshots of live pages, checked against facts
//! measured from the raw page rather than from the engine.
//!
//! The shape fixtures in [`crate::fixture`] are held to the shape contract, which
//! asks every page for landmarks, declared scroll geometry, and similar. A capture
//! is a real page, so it is held to what the page itself says. Every count and
//! title below was measured with Python's `html.parser` over the raw bytes
//! fetched for the capture, and each reduced fixture was checked to reproduce
//! the same numbers before it was committed. The raw pages are not in the
//! repository.
//!
//! Site names are test labels. Nothing in this module, or in the tests that use
//! it, decides behaviour by which site a document belongs to.

use crate::fixture::RealSiteFixture;

/// One reduced real-page capture and the facts measured from its raw page.
#[derive(Clone, Copy, Debug)]
pub struct Capture {
    /// Test label only. Never a decision input.
    pub label: &'static str,
    /// File name under [`crate::fixture::fixture_root`].
    pub html_file: &'static str,
    /// The stylesheet the harness serves for every external stylesheet slot.
    pub css_file: &'static str,
    /// The page URL the capture was fetched from.
    pub base_url: &'static str,
    /// The raw page's `<title>` text.
    pub expected_title: &'static str,
    /// `a[href]` elements in the raw page. The reduced capture keeps all of them.
    pub anchors: usize,
    /// Elements with a `name` among `input`, `textarea`, `select` and `button`.
    pub named_controls: usize,
    /// `input[type=submit]` elements.
    pub submit_inputs: usize,
    /// `img` elements with a `src`, the image resources a load fetches.
    pub images_with_src: usize,
    /// External `<link rel=stylesheet>` elements, whatever their `media`.
    pub stylesheets: usize,
    /// The subset of [`Self::stylesheets`] whose `media` matches a screen. A
    /// `media=print` sheet is discovered and fetched, but does not apply on screen.
    pub screen_stylesheets: usize,
    /// `script` elements with a `src`.
    pub scripts: usize,
    /// `form[role=search]` elements.
    pub search_forms: usize,
    /// `tr.athing` story rows, when the page is a story table.
    pub story_rows: usize,
    /// The first and last `span.titleline > a` text, when the page has story rows.
    pub first_story_title: Option<&'static str>,
    pub last_story_title: Option<&'static str>,
}

impl Capture {
    /// The fixture the harness loads for this capture.
    ///
    /// The shape fields are inert here. A capture is checked by its own facts,
    /// not by the shape contract, so they are left at the values that make the
    /// shape contract inapplicable.
    #[must_use]
    pub fn as_fixture(&self) -> RealSiteFixture {
        RealSiteFixture {
            label: self.label,
            html_file: self.html_file,
            css_file: self.css_file,
            base_url: self.base_url,
            expected_title: self.expected_title,
            expected_stylesheets: self.screen_stylesheets,
            expected_images: self.images_with_src,
            expected_deferred_images: 0,
            expected_srcset_images: 0,
            expected_video_posters: 0,
            expected_scripts: self.scripts,
            min_links: 0,
            min_channel_sections: 0,
            min_scroll_blocks: 0,
            min_ordered_lines: 0,
            min_order_comparisons: 0,
            side_rail: None,
            table: None,
            sticky: None,
            forms: None,
            content_width: 0.0,
            content_left: 0.0,
        }
    }
}

/// The Hacker News front page: a story table of thirty rows.
pub const HACKER_NEWS_HOME: Capture = Capture {
    label: "hacker_news_home",
    html_file: "hacker_news_home.html",
    css_file: "hacker_news_home.css",
    base_url: "https://news.ycombinator.com/",
    expected_title: "Hacker News",
    anchors: 230,
    named_controls: 1,
    submit_inputs: 0,
    images_with_src: 2,
    stylesheets: 1,
    screen_stylesheets: 1,
    scripts: 1,
    search_forms: 0,
    story_rows: 30,
    first_story_title: Some("REA Reverse – Engineer Anything"),
    last_story_title: Some("The Alchemical Transformations of the Mutus Liber (1677)"),
};

/// The Google home page: a search form whose query is a `textarea`, and its
/// submit buttons, with the page's external stylesheets and scripts.
pub const GOOGLE_HOME: Capture = Capture {
    label: "google_home",
    html_file: "google_home.html",
    css_file: "google_home.css",
    base_url: "https://www.google.com/",
    expected_title: "Google",
    anchors: 15,
    named_controls: 11,
    submit_inputs: 4,
    images_with_src: 0,
    stylesheets: 2,
    screen_stylesheets: 1,
    scripts: 2,
    search_forms: 1,
    story_rows: 0,
    first_story_title: None,
    last_story_title: None,
};

/// Every capture the capture tests run over.
pub const CAPTURES: &[Capture] = &[HACKER_NEWS_HOME, GOOGLE_HOME];
