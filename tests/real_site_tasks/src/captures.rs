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
    /// `a[href]` elements in the reduced fixture, counted the way the raw page was.
    /// This is the count the engine is checked against. It equals
    /// [`Self::raw_anchors`] unless the reduction dropped links on purpose.
    pub anchors: usize,
    /// `a[href]` elements in the raw page. Documentation of what was dropped, never
    /// a check: the engine only ever sees the reduced fixture.
    pub raw_anchors: usize,
    /// Elements with a `name` among `input`, `textarea`, `select` and `button`.
    pub named_controls: usize,
    /// `input[type=submit]` elements.
    pub submit_inputs: usize,
    /// `img` elements with a `src` that a scripting-enabled browser fetches: every
    /// `img[src]` outside a `noscript` element, whose content such a browser never
    /// parses as elements.
    pub images_with_src: usize,
    /// `img[src]` elements inside `noscript`, which a scripting-enabled browser
    /// does not fetch.
    pub noscript_images: usize,
    /// External `<link rel=stylesheet>` elements, whatever their `media`.
    pub stylesheets: usize,
    /// The subset of [`Self::stylesheets`] whose `media` matches a screen. A
    /// `media=print` sheet is discovered and fetched, but does not apply on screen.
    pub screen_stylesheets: usize,
    /// `script` elements with a `src` that a module-capable browser runs: every
    /// `script[src]` except the `nomodule` fallbacks, which such a browser skips.
    pub scripts: usize,
    /// `script[src][nomodule]` elements, which the classic plan leaves out.
    pub nomodule_scripts: usize,
    /// True when the page's visible content is produced by its scripts. Such a
    /// capture's static markup lays out no text, so layout expectations do not
    /// apply to it. Its discovery and markup facts still do.
    pub js_rendered_shell: bool,
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
    raw_anchors: 230,
    named_controls: 1,
    submit_inputs: 0,
    images_with_src: 2,
    noscript_images: 0,
    stylesheets: 1,
    screen_stylesheets: 1,
    scripts: 1,
    js_rendered_shell: false,
    nomodule_scripts: 0,
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
    raw_anchors: 15,
    named_controls: 11,
    submit_inputs: 4,
    images_with_src: 0,
    noscript_images: 0,
    stylesheets: 2,
    screen_stylesheets: 1,
    scripts: 2,
    js_rendered_shell: false,
    nomodule_scripts: 0,
    search_forms: 1,
    story_rows: 0,
    first_story_title: None,
    last_story_title: None,
};

/// The Yahoo Hong Kong home page: a dense news portal with a header search form,
/// a category rail, a headline block, weather and sports cards, and a footer.
/// The reduction keeps every link and every script with a `src`.
pub const YAHOO_HK_HOME: Capture = Capture {
    label: "yahoo_hk_home",
    html_file: "yahoo_hk_home.html",
    css_file: "yahoo_hk_home.css",
    base_url: "https://hk.yahoo.com/",
    expected_title: "Yahoo",
    anchors: 71,
    raw_anchors: 71,
    named_controls: 11,
    submit_inputs: 0,
    images_with_src: 34,
    noscript_images: 0,
    stylesheets: 1,
    screen_stylesheets: 1,
    scripts: 21,
    js_rendered_shell: false,
    nomodule_scripts: 1,
    search_forms: 0,
    story_rows: 0,
    first_story_title: None,
    last_story_title: None,
};

/// The YouTube home page: a JavaScript-driven application shell. Its static
/// markup carries the masthead and the guide, and its counts describe that
/// markup, not the page the application renders after its scripts run.
pub const YOUTUBE_HOME: Capture = Capture {
    label: "youtube_home",
    html_file: "youtube_home.html",
    css_file: "youtube_home.css",
    base_url: "https://www.youtube.com/",
    expected_title: "YouTube",
    anchors: 14,
    raw_anchors: 14,
    named_controls: 1,
    submit_inputs: 0,
    images_with_src: 0,
    noscript_images: 0,
    stylesheets: 5,
    screen_stylesheets: 5,
    scripts: 9,
    nomodule_scripts: 0,
    js_rendered_shell: true,
    search_forms: 0,
    story_rows: 0,
    first_story_title: None,
    last_story_title: None,
};

/// The Wikipedia main page: an encyclopedia portal with a header search form, a
/// portal menu, and the featured-content boxes. The reduction drops the
/// interlanguage menu, the page toolbar, and the skip link, which is why the
/// reduced link count is smaller than the raw page's.
pub const WIKIPEDIA_MAIN_PAGE: Capture = Capture {
    label: "wikipedia_main_page",
    html_file: "wikipedia_main_page.html",
    css_file: "wikipedia_main_page.css",
    base_url: "https://en.wikipedia.org/wiki/Main_Page",
    expected_title: "Wikipedia, the free encyclopedia",
    anchors: 243,
    raw_anchors: 645,
    named_controls: 4,
    submit_inputs: 0,
    images_with_src: 22,
    noscript_images: 1,
    stylesheets: 2,
    screen_stylesheets: 2,
    scripts: 1,
    js_rendered_shell: false,
    nomodule_scripts: 0,
    search_forms: 0,
    story_rows: 0,
    first_story_title: None,
    last_story_title: None,
};

/// Every capture the capture tests run over.
pub const CAPTURES: &[Capture] = &[
    HACKER_NEWS_HOME,
    GOOGLE_HOME,
    YAHOO_HK_HOME,
    YOUTUBE_HOME,
    WIKIPEDIA_MAIN_PAGE,
];
