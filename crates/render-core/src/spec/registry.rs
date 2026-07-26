use super::{ConformanceTest, FeatureDefinition, FeatureId, StandardFamily, SupportStatus};

const DOM_TREE: FeatureId = FeatureId::new("dom.tree-mutation");
const HTML_TOKENIZER: FeatureId = FeatureId::new("html.tokenizer");
const HTML_TREE: FeatureId = FeatureId::new("html.tree-construction");
const HTML_ENCODING: FeatureId = FeatureId::new("html.encoding-sniffing");
const CSS_SYNTAX: FeatureId = FeatureId::new("css.syntax");
const CSS_SELECTORS: FeatureId = FeatureId::new("css.selectors");
const CSS_CASCADE: FeatureId = FeatureId::new("css.cascade");
const CSS_VARIABLES: FeatureId = FeatureId::new("css.custom-properties");
const CSS_TYPED_VALUES: FeatureId = FeatureId::new("css.typed-values");
const CSS_USED_VALUES: FeatureId = FeatureId::new("css.used-values");
const CSS_FLEXBOX: FeatureId = FeatureId::new("css.flexbox-single-line");
const CSS_GRID: FeatureId = FeatureId::new("css.grid-explicit-tracks");
const LAYOUT: FeatureId = FeatureId::new("rendering.layout");
const PAINT: FeatureId = FeatureId::new("rendering.paint");
const JS: FeatureId = FeatureId::new("ecmascript.runtime");
const EVENT_LOOP: FeatureId = FeatureId::new("html.event-loop");
const URL_PARSER: FeatureId = FeatureId::new("url.parser");
const NAVIGATION_HISTORY: FeatureId = FeatureId::new("html.navigation-history");
const FETCH: FeatureId = FeatureId::new("fetch.runtime");

const DOM_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "dom::tests"),
    ConformanceTest::new("wpt", "dom/nodes/"),
];
const HTML_TOKENIZER_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "html::tokenizer::tests"),
    ConformanceTest::new("wpt", "html/syntax/parsing/"),
];
const HTML_TREE_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "html::tree_builder::tests"),
    ConformanceTest::new("interop", "tests/fixtures/interop/html_tree_oracle.html"),
];
const HTML_ENCODING_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "html::encoding::tests"),
    ConformanceTest::new("wpt", "encoding/"),
    ConformanceTest::new("wpt", "html/syntax/charset/"),
];
const CSS_SYNTAX_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "css::stylesheet::tests"),
    ConformanceTest::new("wpt", "css/css-syntax/"),
];
const SELECTOR_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "css::selector::tests"),
    ConformanceTest::new("wpt", "css/selectors/"),
];
const CASCADE_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "css::cascade::tests"),
    ConformanceTest::new("wpt", "css/css-cascade/"),
];
const VARIABLE_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "css::computed::tests"),
    ConformanceTest::new("wpt", "css/css-variables/"),
];
const TYPED_VALUE_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "css::properties::tests"),
    ConformanceTest::new("wpt", "css/css-values/"),
];
const LAYOUT_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "layout::tree::tests"),
    ConformanceTest::new("rust", "layout::solver::tests"),
];
const FLEXBOX_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "layout::tree::tests::flex_children"),
    ConformanceTest::new("rust", "layout::solver::tests::single_line"),
    ConformanceTest::new("wpt", "css/css-flexbox/"),
];
const GRID_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new(
        "rust",
        "css::properties::tests::parses_explicit_and_responsive_grid_track_lists",
    ),
    ConformanceTest::new("rust", "layout::grid::tests"),
    ConformanceTest::new("rust", "layout::solver::tests::explicit_grid"),
    ConformanceTest::new("wpt", "css/css-grid/"),
];
const PAINT_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "paint::display_list::tests"),
    ConformanceTest::new("rust", "paint::raster::tests"),
];
const JS_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "js::tests"),
    ConformanceTest::new("test262", "test/"),
];
const EVENT_LOOP_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "event_loop::tests"),
    ConformanceTest::new("wpt", "html/webappapis/scripting/event-loops/"),
];
const URL_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "navigation::tests"),
    ConformanceTest::new("wpt", "url/"),
];
const NAVIGATION_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "navigation::tests"),
    ConformanceTest::new("wpt", "html/browsers/browsing-the-web/"),
];

/// Current, deliberately conservative implementation inventory. A subsystem is
/// not marked conformant until its applicable external suite has been imported
/// and the supported scope has no known failures.
pub static CURRENT_FEATURES: &[FeatureDefinition] = &[
    FeatureDefinition {
        id: DOM_TREE,
        family: StandardFamily::Dom,
        specification: "WHATWG DOM",
        section: "4.2 Trees and 4.5 Mutation algorithms",
        status: SupportStatus::Partial,
        dependencies: &[],
        tests: DOM_TESTS,
    },
    FeatureDefinition {
        id: HTML_TOKENIZER,
        family: StandardFamily::Html,
        specification: "HTML Living Standard",
        section: "13.2.5 Tokenization",
        status: SupportStatus::Partial,
        dependencies: &[],
        tests: HTML_TOKENIZER_TESTS,
    },
    FeatureDefinition {
        id: HTML_ENCODING,
        family: StandardFamily::Html,
        specification: "HTML Living Standard and WHATWG Encoding Standard",
        section: "Determining the character encoding and decoding",
        status: SupportStatus::Partial,
        dependencies: &[],
        tests: HTML_ENCODING_TESTS,
    },
    FeatureDefinition {
        id: HTML_TREE,
        family: StandardFamily::Html,
        specification: "HTML Living Standard",
        section: "13.2.6 Tree construction",
        status: SupportStatus::Partial,
        dependencies: &[DOM_TREE, HTML_TOKENIZER],
        tests: HTML_TREE_TESTS,
    },
    FeatureDefinition {
        id: CSS_SYNTAX,
        family: StandardFamily::Css,
        specification: "CSS Syntax Level 3",
        section: "Tokenization, parsing and error handling",
        status: SupportStatus::Partial,
        dependencies: &[],
        tests: CSS_SYNTAX_TESTS,
    },
    FeatureDefinition {
        id: CSS_SELECTORS,
        family: StandardFamily::Css,
        specification: "Selectors Level 4",
        section: "Selector syntax, matching and specificity",
        status: SupportStatus::Partial,
        dependencies: &[DOM_TREE, CSS_SYNTAX],
        tests: SELECTOR_TESTS,
    },
    FeatureDefinition {
        id: CSS_CASCADE,
        family: StandardFamily::Css,
        specification: "CSS Cascading and Inheritance Level 6",
        section: "Cascade sorting order and defaulting",
        status: SupportStatus::Partial,
        dependencies: &[CSS_SYNTAX, CSS_SELECTORS],
        tests: CASCADE_TESTS,
    },
    FeatureDefinition {
        id: CSS_VARIABLES,
        family: StandardFamily::Css,
        specification: "CSS Custom Properties Level 1",
        section: "Computed-value substitution and cycles",
        status: SupportStatus::Partial,
        dependencies: &[CSS_CASCADE],
        tests: VARIABLE_TESTS,
    },
    FeatureDefinition {
        id: CSS_TYPED_VALUES,
        family: StandardFamily::Css,
        specification: "CSS Values and Units Level 4",
        section: "Property grammars and computed values",
        status: SupportStatus::Partial,
        dependencies: &[CSS_VARIABLES],
        tests: TYPED_VALUE_TESTS,
    },
    FeatureDefinition {
        id: CSS_USED_VALUES,
        family: StandardFamily::Css,
        specification: "CSS Values and Units Level 4",
        section: "Used values and math function resolution",
        status: SupportStatus::Partial,
        dependencies: &[CSS_TYPED_VALUES],
        tests: TYPED_VALUE_TESTS,
    },
    FeatureDefinition {
        id: CSS_FLEXBOX,
        family: StandardFamily::Rendering,
        specification: "CSS Flexible Box Layout Module Level 1",
        section: "Single-line flex containers and flexible lengths",
        status: SupportStatus::Partial,
        dependencies: &[CSS_TYPED_VALUES],
        tests: FLEXBOX_TESTS,
    },
    FeatureDefinition {
        id: CSS_GRID,
        family: StandardFamily::Rendering,
        specification: "CSS Grid Layout Module Level 1",
        section: "Explicit tracks, track sizing, gaps, and auto-placement",
        status: SupportStatus::Partial,
        dependencies: &[CSS_TYPED_VALUES],
        tests: GRID_TESTS,
    },
    FeatureDefinition {
        id: LAYOUT,
        family: StandardFamily::Rendering,
        specification: "CSS Display and CSS Box specifications",
        section: "Formatting structure and layout",
        status: SupportStatus::Partial,
        dependencies: &[DOM_TREE, CSS_USED_VALUES, CSS_FLEXBOX, CSS_GRID],
        tests: LAYOUT_TESTS,
    },
    FeatureDefinition {
        id: PAINT,
        family: StandardFamily::Rendering,
        specification: "CSS 2 and CSS Painting specifications",
        section: "Painting order and stacking contexts",
        status: SupportStatus::Partial,
        dependencies: &[LAYOUT],
        tests: PAINT_TESTS,
    },
    FeatureDefinition {
        id: JS,
        family: StandardFamily::EcmaScript,
        specification: "ECMAScript Language Specification",
        section: "Execution contexts, objects and jobs",
        status: SupportStatus::Partial,
        dependencies: &[],
        tests: JS_TESTS,
    },
    FeatureDefinition {
        id: EVENT_LOOP,
        family: StandardFamily::Html,
        specification: "HTML Living Standard",
        section: "8.1.7 Event loops",
        status: SupportStatus::Partial,
        dependencies: &[DOM_TREE, JS],
        tests: EVENT_LOOP_TESTS,
    },
    FeatureDefinition {
        id: URL_PARSER,
        family: StandardFamily::Url,
        specification: "WHATWG URL Standard",
        section: "URL parsing, serialization and relative resolution",
        status: SupportStatus::Partial,
        dependencies: &[],
        tests: URL_TESTS,
    },
    FeatureDefinition {
        id: NAVIGATION_HISTORY,
        family: StandardFamily::Html,
        specification: "HTML Living Standard",
        section: "7.4 Navigation and 7.2.6 Session history traversal",
        status: SupportStatus::Partial,
        dependencies: &[URL_PARSER],
        tests: NAVIGATION_TESTS,
    },
    FeatureDefinition {
        id: FETCH,
        family: StandardFamily::Fetch,
        specification: "Fetch Standard",
        section: "Fetching, CORS and HTTP-network fetch",
        status: SupportStatus::Missing,
        dependencies: &[EVENT_LOOP],
        tests: &[],
    },
];
