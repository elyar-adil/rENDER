//! Fixture registry: the reduced real-site page shapes and how to load them.
//!
//! A fixture is a *page shape*, not a capture and not an alternate runtime
//! implementation. It keeps the URL shapes a real page uses and the resource
//! kinds a real page uses; the harness ([`crate::harness`]) answers every
//! external request with a deterministic local value, so no check in this crate
//! requires Internet access.
//!
//! Each fixture was reduced from a real page that was fetched and read: the
//! markup shape, the class names, the resource shapes and the table or form
//! shape are kept, and the tracking parameters, the payloads, the web-component
//! shadow roots and the enormous parts are dropped. Every fixture's own
//! stylesheet is the deterministic local response for its external sheets, so
//! the gate never needs the network.
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

/// A two-column page's side rail.
///
/// The rail is named by a **selector** rather than by a class, so the check
/// works on any page that has one and is skipped entirely on a page that does
/// not. Nothing here says how the two columns must be produced: `display: flex`,
/// grid, floats and absolute positioning are all correct answers, and the
/// contract only constrains the result.
#[derive(Clone, Copy, Debug)]
pub struct SideRailShape {
    /// Selects the rail, which must match exactly one element.
    pub rail_selector: &'static str,
}

/// A page carrying a normative-style data table.
#[derive(Clone, Copy, Debug)]
pub struct TableShape {
    /// Selects the table, or every table on the page.
    pub selector: &'static str,
    /// How many elements `selector` must match.
    pub count: usize,
    /// Lower bound on cells carrying `colspan` or `rowspan`.
    ///
    /// This is what stops the merged-cell geometry check from passing on a table
    /// that has no merged cells: the comparison would be vacuous, which is the
    /// failure mode this round is specifically looking for.
    pub min_merged_cells: usize,
    /// Lower bound on `caption` elements.
    pub captions: usize,
    /// Lower bound on `colgroup` elements.
    pub colgroups: usize,
}

/// A sticky element, a nested scrollport, and a sticky element inside that
/// scrollport.
#[derive(Clone, Copy, Debug)]
pub struct StickyShape {
    /// Selects the sticky box, which must match exactly one element.
    pub sticky_selector: &'static str,
    /// The `top` inset the fixture's own stylesheet declares for it.
    pub top_inset: f32,
    /// Selects the nested scrollport, which must have horizontal range.
    pub nested_scroll_selector: &'static str,
    /// Selects a sticky element *inside* the nested scrollport.
    ///
    /// Its presence is asserted, and the open question - which scrollport its
    /// constraint resolves against - is an ignored test, because the engine's
    /// answer is the root viewport and that is a recorded limitation rather than
    /// a contract requirement.
    pub sticky_in_scrollport_selector: &'static str,
}

/// The form-owner shapes a fixture must preserve.
///
/// `controls` pairs a **role** with the selector for the controls in it, so the
/// contract can say what a role *means* once and every fixture reuses it.
#[derive(Clone, Copy, Debug)]
pub struct FormShape {
    pub controls: &'static [(&'static str, &'static str)],
    /// Pairs a form selector with the lower bound on named controls it owns.
    ///
    /// Counting only the members with a `name` matters: the count is what proves
    /// a control that is *elsewhere in the tree* is in the collection, since a
    /// control inside the form would be there under any reading.
    pub members: &'static [(&'static str, usize)],
}

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
    /// Lower bound on laid-out text lines for the whole page, so the
    /// document-order assertion has something to compare.
    ///
    /// This is a guard against the failure mode the previous round could not see:
    /// an ordering check over a collection that is empty passes. A fixture whose
    /// layout collapsed to a handful of lines would satisfy every other check
    /// here, so each one declares how much text it expects to find.
    pub min_ordered_lines: usize,
    /// Lower bound on the number of **order relations** the per-column
    /// document-order assertion must actually check.
    ///
    /// This is the anti-vacuity floor, and it is a count of comparisons rather
    /// than a count of lines on purpose. Most of a real page's text lines are
    /// one-line paragraphs with no other line in their column to be compared
    /// against, so a share-of-lines requirement is not reachable by correct
    /// layout. What a vacuous check looks like is a small number of comparisons
    /// over a large number of lines, and that is what this bounds. Measured
    /// values are 20-120 per fixture, so the floors are set well below them:
    /// a floor that has to be raised every time a fixture gains a sentence is a
    /// floor nobody will keep.
    pub min_order_comparisons: usize,
    /// The side rail, when the page has one. `None` for a single-column page.
    pub side_rail: Option<SideRailShape>,
    /// The table shape, when the page has one.
    pub table: Option<TableShape>,
    /// The sticky and scrollport shapes, when the page has one.
    pub sticky: Option<StickyShape>,
    /// The form-owner shapes, when the page has one.
    pub forms: Option<FormShape>,
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
    min_ordered_lines: 40,
    min_order_comparisons: 20,
    content_width: 1264.0,
    content_left: 8.0,
    side_rail: None,
    table: None,
    sticky: None,
    forms: None,
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
    min_ordered_lines: 30,
    min_order_comparisons: 20,
    content_width: 1264.0,
    content_left: 8.0,
    side_rail: None,
    table: None,
    sticky: None,
    forms: None,
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
    min_ordered_lines: 40,
    min_order_comparisons: 20,
    content_width: 1000.0,
    content_left: 140.0,
    side_rail: None,
    table: None,
    sticky: None,
    forms: None,
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
    min_ordered_lines: 20,
    min_order_comparisons: 20,
    content_width: 1000.0,
    content_left: 140.0,
    side_rail: None,
    table: None,
    sticky: None,
    // The article page keeps a comment composer next to its search form, so a
    // gate that keys on "there is a form" instead of the landmark role fails
    // here. Only the ancestor case is declared, because that is all this page
    // has; the roles that need a non-ancestor owner are the form fixture's.
    forms: Some(FormShape {
        controls: &[
            ("ancestor", "form:not([role=search]) input[name]"),
            ("ancestor", "form:not([role=search]) button"),
        ],
        members: &[("form:not([role=search])", 1)],
    }),
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
    min_ordered_lines: 50,
    min_order_comparisons: 20,
    content_width: 1280.0,
    content_left: 0.0,
    side_rail: None,
    table: None,
    sticky: None,
    forms: None,
};

/// A documentation reference page: a main column of sections with a real side
/// rail *beside* it, not below it.
///
/// This is the fixture that made the per-column document-order scoping
/// necessary, and the reason is recorded on the page itself. `content_width` and
/// `content_left` are still the `<main>` wrapper's, so the two columns are
/// asserted separately by `contract::column_shapes`.
pub const REFERENCE_TWO_COLUMN: RealSiteFixture = RealSiteFixture {
    label: "reference_two_column",
    html_file: "reference_two_column.html",
    css_file: "reference_two_column.css",
    base_url: "https://developer.mozilla.org/zh-CN/docs/Web/CSS/position",
    expected_title: "position 属性 - CSS 参考 | 开发者文档",
    // Two sheets on one static origin, so every stylesheet diagnostic is
    // produced once per slot and the diagnostic-set assertion is exact.
    expected_stylesheets: 2,
    // The site logo and the containing-block figure.
    expected_images: 2,
    expected_deferred_images: 0,
    expected_srcset_images: 0,
    expected_video_posters: 0,
    expected_scripts: 1,
    min_links: 34,
    min_channel_sections: 2,
    min_scroll_blocks: 10,
    min_ordered_lines: 40,
    min_order_comparisons: 20,
    content_width: 1264.0,
    content_left: 8.0,
    side_rail: Some(SideRailShape {
        rail_selector: "aside.page-rail",
    }),
    table: None,
    sticky: None,
    forms: None,
};

/// A specification page whose centrepiece is a dense normative table: row
/// groups, a caption, `colspan`/`rowspan` merged cells and two `colgroup`s.
pub const SPEC_DATA_TABLE: RealSiteFixture = RealSiteFixture {
    label: "spec_data_table",
    html_file: "spec_data_table.html",
    css_file: "spec_data_table.css",
    base_url: "https://www.w3.org/TR/css-position-3/",
    expected_title: "CSS 定位布局 第 3 级规范 - 绝对定位盒模型取值汇总",
    // A base sheet and a normative-table sheet, as the live page really has.
    expected_stylesheets: 2,
    // The organisation logos in the header and the contributing figure.
    expected_images: 2,
    expected_deferred_images: 0,
    expected_srcset_images: 0,
    expected_video_posters: 0,
    expected_scripts: 1,
    min_links: 30,
    min_channel_sections: 2,
    // The table's `tbody` rows outnumber the prose clauses, so the scroll region
    // the contract picks is the table itself. That is deliberate: the ordered
    // scroll-block assertion then checks that rows stack in document order.
    min_scroll_blocks: 12,
    min_ordered_lines: 40,
    min_order_comparisons: 20,
    content_width: 1264.0,
    content_left: 8.0,
    side_rail: Some(SideRailShape {
        rail_selector: "aside.page-rail",
    }),
    table: Some(TableShape {
        // Both tables on the page, which is the point: one six-column table with
        // `colspan` and `rowspan` and row groups, and one eight-column index
        // table with `th scope=row` row headers.
        selector: "table",
        count: 2,
        // 4 + 7 + 1 in the rules table, 0 in the index table.
        min_merged_cells: 11,
        captions: 2,
        colgroups: 2,
    }),
    sticky: None,
    forms: None,
};

/// A documentation page with a sticky top bar that contains a nested
/// horizontal scrollport, over content several screens long.
pub const STICKY_TOOLBAR: RealSiteFixture = RealSiteFixture {
    label: "sticky_toolbar",
    html_file: "sticky_toolbar.html",
    css_file: "sticky_toolbar.css",
    base_url: "https://kubernetes.io/zh-cn/docs/tasks/configure-pod-container/assign-memory-resource/",
    expected_title: "为容器和 Pod 分配内存资源 - Kubernetes 文档",
    // A framework bundle and a page sheet, as the live page really has.
    expected_stylesheets: 2,
    // The container-resources figure. The brand mark is an inline `svg`, which
    // carries a viewBox and no width/height on purpose.
    expected_images: 1,
    expected_deferred_images: 0,
    expected_srcset_images: 0,
    expected_video_posters: 0,
    expected_scripts: 1,
    min_links: 26,
    min_channel_sections: 0,
    min_scroll_blocks: 11,
    min_ordered_lines: 35,
    min_order_comparisons: 20,
    content_width: 1240.0,
    content_left: 20.0,
    side_rail: None,
    table: None,
    sticky: Some(StickyShape {
        sticky_selector: "header.site-head",
        top_inset: 0.0,
        nested_scroll_selector: ".navbar-scroll",
        sticky_in_scrollport_selector: ".tab-sticky",
    }),
    forms: None,
};

/// A sign-in and account page: a search form, a login form, a second form, and
/// controls in no form, in one form, or owned by a form that is not their
/// ancestor.
pub const FORM_HEAVY_SIGNIN: RealSiteFixture = RealSiteFixture {
    label: "form_heavy_signin",
    html_file: "form_heavy_signin.html",
    css_file: "form_heavy_signin.css",
    base_url: "https://passport.csdn.net/login?code=public",
    expected_title: "登录 - 会员中心",
    // A member sheet and a form sheet, as the live page really has.
    expected_stylesheets: 2,
    // The member-centre logo.
    expected_images: 1,
    expected_deferred_images: 0,
    expected_srcset_images: 0,
    expected_video_posters: 0,
    expected_scripts: 1,
    min_links: 26,
    min_channel_sections: 2,
    min_scroll_blocks: 14,
    min_ordered_lines: 40,
    min_order_comparisons: 20,
    content_width: 940.0,
    content_left: 8.0,
    side_rail: Some(SideRailShape {
        rail_selector: "aside.page-rail",
    }),
    table: None,
    sticky: None,
    forms: Some(FormShape {
        controls: &[
            // The nearest-ancestor step: a control inside a form, owned by it.
            ("ancestor", "#acct"),
            ("ancestor", "#pw"),
            ("ancestor", "#news-topic"),
            // Step 2 decides the owner, and the owner is *not* an ancestor. This
            // is the case a "nearest ancestor form" walk gets wrong.
            ("named", "#news-email"),
            // Step 2 is an if/else, not a fallback: an unresolved `form`
            // attribute means no owner, even inside a form.
            ("broken", "#broken-ref"),
            // No form anywhere above it.
            ("orphan", "#orphan-locale"),
            ("orphan", "#orphan-theme"),
            ("orphan", "#orphan-digest"),
            // The HTML parser's form element pointer. This control is
            // foster-parented out of the table, so the form that owns it is its
            // sibling and the finished tree cannot express the association.
            ("parser", "#scope"),
            // A control in the same table's cells, which has **no** owner: the
            // pointer was cleared by the form's end tag before these were
            // inserted. Two controls, two different mechanisms, two different
            // answers - and the second one is the one a "nearest ancestor form"
            // walk also gets right, for the wrong reason.
            ("orphan", "#tag-name"),
            ("orphan", "#tag-note"),
        ],
        members: &[
            // The login form owns its own controls *and* the newsletter's email
            // field, which is elsewhere in the tree. Counting only the named
            // members is what makes "elsewhere in the tree" load-bearing.
            ("#login", 6),
            ("#newsletter", 4),
            // The foster-parented form owns exactly the one control that the
            // parser pointed it at.
            ("#quick-tag", 1),
        ],
    }),
};

/// The five fixtures `docs/real_site_acceptance.md` originally listed, in the
/// order it lists them, followed by the four page shapes this round added.
///
/// The second group exists because everything that landed in the engine this
/// session - CSS 2.1 section 17 table layout, `position: sticky` and scrollport
/// geometry, the derived form owner, `@supports` evaluation, at-rule and
/// declaration-list diagnostics - is multi-column, nested or table-shaped, and
/// none of it is visible to a single-column page. The first group stays as it
/// is: it is the regression net, and the two groups are kept adjacent so the
/// split between "was always here" and "was added because something landed" is
/// one line to read.
pub const FIXTURES: &[RealSiteFixture] = &[
    BAIDU_HOME,
    BAIDU_RESULTS,
    ZHIHU_HOME,
    ZHIHU_ARTICLE,
    NETEASE_163_HOME,
    REFERENCE_TWO_COLUMN,
    SPEC_DATA_TABLE,
    STICKY_TOOLBAR,
    FORM_HEAVY_SIGNIN,
];

/// The fixtures the acceptance document named when the harness was written.
pub const ORIGINAL_FIXTURES: &[&str] = &[
    "baidu_home",
    "baidu_results",
    "zhihu_home",
    "zhihu_article",
    "netease_163_home",
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
    use super::{
        CONTRACT_VIEWPORT_WIDTH, FIXTURES, ORIGINAL_FIXTURES, baseline_root, by_label, fixture_root,
    };

    #[test]
    fn fixture_registry_is_addressable_and_unique() {
        assert_eq!(FIXTURES.len(), 9);
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

    /// The original five stay first and stay in the order the acceptance
    /// document lists them, so a diff shows the additions rather than a
    /// reshuffle.
    #[test]
    fn the_original_five_are_still_first_and_in_order() {
        let leading: Vec<&str> = FIXTURES
            .iter()
            .take(ORIGINAL_FIXTURES.len())
            .map(|fixture| fixture.label)
            .collect();
        assert_eq!(leading, ORIGINAL_FIXTURES);
    }

    /// Every fixture must carry a non-ASCII title, because that is the only
    /// place the HTML encoding path is observable: a fixture whose title is
    /// all-ASCII would pass a broken decoder. `tests/test_real_site_capabilities.py`
    /// asserts the same property, and this is the same check on the Rust side so
    /// the two gates cannot drift apart.
    #[test]
    fn every_fixture_title_is_non_ascii_so_decoding_is_observable() {
        for fixture in FIXTURES {
            assert!(
                fixture
                    .expected_title
                    .chars()
                    .any(|character| character > '\u{7f}'),
                "{}: the expected title has no non-ASCII character, so the encoding path \
                 is not observable through it",
                fixture.label
            );
        }
    }

    /// Every fixture must reach the stylesheet-geometry assertion with numbers
    /// the fixture's own stylesheet declares. A zero width would let the
    /// assertion pass for a page that laid out nothing.
    #[test]
    fn every_fixture_declares_a_positive_content_column() {
        for fixture in FIXTURES {
            assert!(
                fixture.content_width > 0.0,
                "{}: content_width must be positive",
                fixture.label
            );
            assert!(
                fixture.content_left >= 0.0,
                "{}: content_left must not be negative",
                fixture.label
            );
            assert!(
                fixture.content_left + fixture.content_width <= CONTRACT_VIEWPORT_WIDTH,
                "{}: the declared content column runs past the {CONTRACT_VIEWPORT_WIDTH}px \
                 contract viewport",
                fixture.label
            );
        }
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
