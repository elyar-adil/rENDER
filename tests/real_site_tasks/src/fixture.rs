//! Fixture registry: the five reduced real-site page shapes and how to load them.
//!
//! A fixture is a *page shape*, not a capture and not an alternate runtime
//! implementation. It keeps the URL shapes a real page uses and the resource
//! kinds a real page uses; the harness ([`crate::harness`]) answers every
//! external request with a deterministic local value, so no check in this crate
//! requires Internet access.
//!
//! The `label` on each fixture is a test label. Nothing in this crate branches
//! on it: every rule below is read from the fixture's own HTML and CSS and
//! satisfied through the engine's normal HTML, CSS, layout, paint, and resource
//! paths. See `docs/generic-browser-todo.md` and `tools/check_site_neutrality.py`.

use std::fs;
use std::path::{Path, PathBuf};

use url::Url;

/// The viewport every contract check is measured against.
///
/// The height is the 600px first-screen bound the acceptance contract names:
/// every fixture must lay out past it.
pub const CONTRACT_VIEWPORT_WIDTH: f32 = 1_280.0;
pub const CONTRACT_VIEWPORT_HEIGHT: f32 = 600.0;

/// One reduced real-site page shape and the contract it pins down.
#[derive(Clone, Copy, Debug)]
pub struct RealSiteFixture {
    /// Test label only. Never a decision input.
    pub label: &'static str,
    /// File name under [`fixture_root`].
    pub html_file: &'static str,
    /// Local deterministic stand-in for every external stylesheet response.
    pub css_file: &'static str,
    /// The page URL the fixture is a snapshot of. Handed to the engine as the
    /// document base URL so URL resolution is exercised, never compared.
    pub base_url: &'static str,
    /// The document title, after HTML encoding sniffing and parsing.
    pub expected_title: &'static str,
    /// Eligible external `<link rel=stylesheet>` slots.
    pub expected_stylesheets: usize,
    /// Image resources the engine is expected to fetch: `img[src]`,
    /// `img[srcset]`, and `video[poster]`.
    pub expected_images: usize,
    /// `img` elements that carry a deferred `data-src`/`data-original` pair and
    /// no `src`/`srcset` yet. The engine must report these as
    /// `MissingSource`, and must not fail on them.
    pub expected_deferred_images: usize,
    /// `img` elements whose only source is a `srcset` candidate list.
    pub expected_srcset_images: usize,
    /// `video[poster]` elements.
    pub expected_video_posters: usize,
    /// Discovered scripts, all external and deferred.
    pub expected_scripts: usize,
    /// Lower bound on navigable `a[href]` elements.
    pub min_links: usize,
    /// Lower bound on `section` elements that carry at least three links.
    pub min_channel_sections: usize,
    /// Lower bound on ordered text blocks in the scroll region.
    pub min_scroll_blocks: usize,
    /// Border-box width the fixture's own external stylesheet declares for the
    /// `<main>` content column. Asserted after layout, so it proves the
    /// supplied external stylesheet reached the engine rather than only the
    /// stylesheet slot being discovered.
    pub content_width: f32,
    /// Document-space `x` the same rule declares for that column.
    pub content_left: f32,
}

pub const BAIDU_HOME: RealSiteFixture = RealSiteFixture {
    label: "baidu_home",
    html_file: "baidu_home.html",
    css_file: "baidu_home.css",
    base_url: "https://www.baidu.com/",
    expected_title: "百度一下，你就知道",
    expected_stylesheets: 1,
    // The header logo and the App download code.
    expected_images: 2,
    expected_deferred_images: 0,
    expected_srcset_images: 0,
    expected_video_posters: 0,
    expected_scripts: 1,
    min_links: 30,
    min_channel_sections: 2,
    min_scroll_blocks: 20,
    content_width: 1264.0,
    content_left: 8.0,
};

pub const BAIDU_RESULTS: RealSiteFixture = RealSiteFixture {
    label: "baidu_results",
    html_file: "baidu_results.html",
    css_file: "baidu_results.css",
    base_url: "https://www.baidu.com/s?wd=%E6%B5%8F%E8%A7%88%E5%99%A8",
    expected_title: "前端布局引擎_百度搜索",
    expected_stylesheets: 1,
    // The header logo and the credibility mark.
    expected_images: 2,
    expected_deferred_images: 0,
    expected_srcset_images: 0,
    expected_video_posters: 0,
    expected_scripts: 1,
    min_links: 25,
    min_channel_sections: 2,
    min_scroll_blocks: 12,
    content_width: 1264.0,
    content_left: 8.0,
};

pub const ZHIHU_HOME: RealSiteFixture = RealSiteFixture {
    label: "zhihu_home",
    html_file: "zhihu_home.html",
    css_file: "zhihu_home.css",
    base_url: "https://www.zhihu.com/",
    expected_title: "知乎 - 有问题，就会有答案",
    expected_stylesheets: 1,
    // The header logo and the followed-avatar.
    expected_images: 2,
    expected_deferred_images: 0,
    expected_srcset_images: 0,
    expected_video_posters: 0,
    expected_scripts: 1,
    min_links: 40,
    min_channel_sections: 2,
    min_scroll_blocks: 20,
    content_width: 1000.0,
    content_left: 140.0,
};

pub const ZHIHU_ARTICLE: RealSiteFixture = RealSiteFixture {
    label: "zhihu_article",
    html_file: "zhihu_article.html",
    css_file: "zhihu_article.css",
    base_url: "https://zhuanlan.zhihu.com/p/600000001",
    expected_title: "用确定性测量给渲染结果建立像素基线 - 知乎专栏",
    expected_stylesheets: 1,
    // Column logo, the article cover, the responsive figure, and the avatar.
    expected_images: 4,
    expected_deferred_images: 0,
    expected_srcset_images: 1,
    expected_video_posters: 0,
    expected_scripts: 1,
    min_links: 14,
    min_channel_sections: 2,
    min_scroll_blocks: 4,
    content_width: 1000.0,
    content_left: 140.0,
};

pub const NETEASE_163_HOME: RealSiteFixture = RealSiteFixture {
    label: "netease_163_home",
    html_file: "netease_163_home.html",
    css_file: "netease_163_home.css",
    base_url: "https://www.163.com/",
    expected_title: "网易新闻 - 网易",
    // Two external sheets on the same static origin, so DOM-order cascade of
    // separate external responses is part of the contract.
    expected_stylesheets: 2,
    // Header logo, lead image, two `srcset` figures, the subscription code,
    // and the video poster. The three `img.lazy` elements are not in this
    // count: they declare no `src` and no `srcset` yet.
    expected_images: 6,
    // Three `img.lazy` elements with `data-src`/`data-original` and no `src`.
    expected_deferred_images: 3,
    expected_srcset_images: 2,
    expected_video_posters: 1,
    expected_scripts: 1,
    min_links: 60,
    min_channel_sections: 6,
    min_scroll_blocks: 24,
    content_width: 1280.0,
    content_left: 0.0,
};

/// The five fixtures, in the order `docs/real_site_acceptance.md` lists them.
pub const FIXTURES: &[RealSiteFixture] = &[
    BAIDU_HOME,
    BAIDU_RESULTS,
    ZHIHU_HOME,
    ZHIHU_ARTICLE,
    NETEASE_163_HOME,
];

/// Look a fixture up by its test label.
#[must_use]
pub fn by_label(label: &str) -> Option<&'static RealSiteFixture> {
    FIXTURES.iter().find(|fixture| fixture.label == label)
}

/// Directory holding the fixture pages and their deterministic responses.
#[must_use]
pub fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("fixtures")
        .join("real_sites")
}

/// Directory holding human-verified pixel baselines. Empty until a human
/// promotes one; see `docs/real_site_acceptance.md`.
#[must_use]
pub fn baseline_root() -> PathBuf {
    fixture_root().join("baselines")
}

/// Baseline PNG for one fixture label.
#[must_use]
pub fn baseline_path(label: &str) -> PathBuf {
    baseline_root().join(format!("{label}.png"))
}

/// Fixture bytes and the deterministic responses that answer its resources.
#[derive(Clone, Debug)]
pub struct FixtureSource {
    /// Raw page bytes, exactly as they would arrive over the wire. Decoding
    /// these through the HTML encoding algorithm is part of the contract.
    pub html_bytes: Vec<u8>,
    /// The local bytes served for every external stylesheet request.
    pub css: String,
    /// The document base URL, parsed from [`RealSiteFixture::base_url`].
    pub base_url: Url,
}

impl FixtureSource {
    /// Read a fixture from disk.
    ///
    /// # Panics
    ///
    /// Panics when the fixture files are missing or the declared base URL does
    /// not parse. Both are build-time authoring errors, not test outcomes.
    #[must_use]
    pub fn read(fixture: &RealSiteFixture) -> Self {
        let root = fixture_root();
        let html_path = root.join(fixture.html_file);
        let css_path = root.join(fixture.css_file);
        let html_bytes = fs::read(&html_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", html_path.display()));
        let css = fs::read_to_string(&css_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", css_path.display()));
        let base_url = Url::parse(fixture.base_url)
            .unwrap_or_else(|error| panic!("{} is not a valid base URL: {error}", fixture.label));
        Self {
            html_bytes,
            css,
            base_url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FIXTURES, baseline_root, by_label, fixture_root};

    #[test]
    fn fixture_registry_is_addressable_and_unique() {
        assert_eq!(FIXTURES.len(), 5);
        let mut labels: Vec<&str> = FIXTURES.iter().map(|f| f.label).collect();
        labels.sort_unstable();
        let unique = {
            let mut copy = labels.clone();
            copy.dedup();
            copy
        };
        assert_eq!(labels, unique, "fixture labels must be unique");
        for fixture in FIXTURES {
            assert_eq!(
                by_label(fixture.label).map(|f| f.label),
                Some(fixture.label)
            );
        }
        assert!(by_label("no_such_fixture").is_none());
    }

    #[test]
    fn every_fixture_file_is_present() {
        let root = fixture_root();
        for fixture in FIXTURES {
            assert!(
                root.join(fixture.html_file).is_file(),
                "{} is missing",
                fixture.html_file
            );
            assert!(
                root.join(fixture.css_file).is_file(),
                "{} is missing",
                fixture.css_file
            );
        }
    }

    #[test]
    fn no_baseline_is_checked_in_yet() {
        // Baselines are a human decision, made through `real-site-shots
        // --promote` after someone has looked at a rendered PNG. The harness
        // ships without one, so every render_diff row reads `no-baseline`.
        assert!(
            !baseline_root().join("baidu_home.png").exists(),
            "a pixel baseline was promoted; docs/real_site_acceptance.md must record \
             the review that approved it"
        );
    }
}
