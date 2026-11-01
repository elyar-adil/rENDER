use super::{ConformanceTest, FeatureDefinition, FeatureId, StandardFamily, SupportStatus};

const DOM_TREE: FeatureId = FeatureId::new("dom.tree-mutation");
const HTML_TOKENIZER: FeatureId = FeatureId::new("html.tokenizer");
const HTML_TREE: FeatureId = FeatureId::new("html.tree-construction");
const HTML_FOREIGN: FeatureId = FeatureId::new("html.foreign-content");
const HTML_TEMPLATE: FeatureId = FeatureId::new("html.template-contents");
const HTML_SCRIPTING_MODE: FeatureId = FeatureId::new("html.scripting-mode");
const HTML_FORMS: FeatureId = FeatureId::new("html.forms");
const HTML_QUIRKS: FeatureId = FeatureId::new("html.quirks-mode");
const HTML_ENCODING: FeatureId = FeatureId::new("html.encoding-sniffing");
const CSS_SYNTAX: FeatureId = FeatureId::new("css.syntax");
const CSS_SELECTORS: FeatureId = FeatureId::new("css.selectors");
const CSS_PSEUDO_ELEMENTS: FeatureId = FeatureId::new("css.pseudo-elements");
const CSS_CASCADE: FeatureId = FeatureId::new("css.cascade");
const CSS_NESTING: FeatureId = FeatureId::new("css.nesting");
const CSS_VARIABLES: FeatureId = FeatureId::new("css.custom-properties");
const CSS_TYPED_VALUES: FeatureId = FeatureId::new("css.typed-values");
const CSS_COLORS: FeatureId = FeatureId::new("css.colors");
const CSS_USED_VALUES: FeatureId = FeatureId::new("css.used-values");
const CSS_MEDIA: FeatureId = FeatureId::new("css.media-queries");
const CSS_FONT_FACE: FeatureId = FeatureId::new("css.font-face");
const CSS_ANIMATIONS: FeatureId = FeatureId::new("css.animations");
const CSS_TRANSITIONS: FeatureId = FeatureId::new("css.transitions");
const CSS_TEXT_DECORATION: FeatureId = FeatureId::new("css.text-decoration");
const CSS_FLEXBOX: FeatureId = FeatureId::new("css.flexbox-single-line");
const CSS_GRID: FeatureId = FeatureId::new("css.grid-explicit-tracks");
const LAYOUT: FeatureId = FeatureId::new("rendering.layout");
const CSS_TABLES: FeatureId = FeatureId::new("css.tables");
const SVG_INLINE: FeatureId = FeatureId::new("svg.inline-rasterization");
const PAINT: FeatureId = FeatureId::new("rendering.paint");
const JS: FeatureId = FeatureId::new("ecmascript.runtime");
const EVENT_LOOP: FeatureId = FeatureId::new("html.event-loop");
const URL_PARSER: FeatureId = FeatureId::new("url.parser");
const NAVIGATION_HISTORY: FeatureId = FeatureId::new("html.navigation-history");
const FETCH: FeatureId = FeatureId::new("fetch.runtime");
const VIDEO_DECODE: FeatureId = FeatureId::new("media.video-decode");

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
const HTML_FOREIGN_TESTS: &[ConformanceTest] = &[ConformanceTest::new(
    "rust",
    "html::tree_builder::tests::foreign_content",
)];
const HTML_TEMPLATE_TESTS: &[ConformanceTest] = &[ConformanceTest::new(
    "rust",
    "html::tree_builder::tests::template",
)];
const HTML_QUIRKS_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "document::tests::quirks_mode"),
    ConformanceTest::new("wpt", "css/css-misc/quirks/"),
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
const TABLE_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "layout::solver::table_tests"),
    ConformanceTest::new("wpt", "css/css-tables/"),
];
const SVG_TESTS: &[ConformanceTest] = &[
    ConformanceTest::new("rust", "image::inline_svg::tests"),
    ConformanceTest::new("rust", "image::svg::tests"),
    ConformanceTest::new("wpt", "svg/"),
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
const VIDEO_TESTS: &[ConformanceTest] = &[ConformanceTest::new("rust", "js::video::tests")];

/// Current implementation inventory.
///
/// **On the statuses.** `Conformant` is claimed for nothing here, and that is a
/// decision rather than an omission. The bar this file has always stated is
/// that "a subsystem is not marked conformant until its applicable external
/// suite has been imported and the supported scope has no known failures", and
/// no WPT area is imported. `Partial` and `Missing` are the only honest
/// answers today, so they are the only ones used, and every entry carries a
/// [`FeatureDefinition::notes`] saying which half is built.
///
/// **`Missing` versus `Partial`** is the distinction that carries information.
/// `Missing` means the engine has nothing: a global that is absent, a pipeline
/// stage that was never written. `Partial` means something works and something
/// does not, and the note says which.
pub static CURRENT_FEATURES: &[FeatureDefinition] = &[
    FeatureDefinition {
        id: DOM_TREE,
        family: StandardFamily::Dom,
        specification: "WHATWG DOM",
        section: "4.2 Trees and 4.5 Mutation algorithms",
        status: SupportStatus::Partial,
        notes: "Tree construction, traversal, attribute and character-data mutation, \
                live NodeList/HTMLCollection-style views and a mutation history are \
                implemented. `customElements` is implemented for autonomous elements \
                (define/get/getName/whenDefined/upgrade, upgrade on define and on \
                insertion, connected/disconnected/attributeChanged callbacks for \
                the attribute and subtree APIs); customized built-ins, \
                `classList`/`style`/`dataset` attribute reactions and declarative \
                shadow roots (`shadowrootmode`) are absent. Shadow trees are \
                implemented: `attachShadow` with `mode`, `delegatesFocus` and \
                `slotAssignment`, `shadowRoot`, the shadow-including root and \
                `isConnected`, `getRootNode`, named and manual slot assignment, \
                retargeted event targets and `relatedTarget`, `composedPath`, the \
                flat tree that style and layout walk, and image and video discovery \
                through the shadow-including tree. `slotchange` fires after a \
                slot's assigned nodes change, and author stylesheets declared \
                inside a shadow tree are collected and scoped to that tree; \
                `:host`, `:host()`, `:host-context()` and `::slotted()` match \
                through the cascade. `::part()`, `:has-slotted()`, declarative \
                shadow roots (`shadowrootmode`) and `::slotted()::before/after` \
                are absent.",
        dependencies: &[],
        tests: DOM_TESTS,
    },
    FeatureDefinition {
        id: HTML_TOKENIZER,
        family: StandardFamily::Html,
        specification: "HTML Living Standard",
        section: "13.2.5 Tokenization",
        status: SupportStatus::Partial,
        notes: "The full tokenizer including RCDATA and RAWTEXT states, the script \
                data double-escape states and the CDATA section is implemented. \
                Preprocessor and encoding-change behaviour is covered by \
                `html.encoding-sniffing` rather than here.",
        dependencies: &[],
        tests: HTML_TOKENIZER_TESTS,
    },
    FeatureDefinition {
        id: HTML_ENCODING,
        family: StandardFamily::Html,
        specification: "HTML Living Standard and WHATWG Encoding Standard",
        section: "Determining the character encoding and decoding",
        status: SupportStatus::Partial,
        notes: "BOM and meta prescan, the label table, the transport-layer override \
                and UTF-16 decode are implemented. A label with no confidence and a \
                locale-dependent fallback are not distinguished.",
        dependencies: &[],
        tests: HTML_ENCODING_TESTS,
    },
    FeatureDefinition {
        id: HTML_TREE,
        family: StandardFamily::Html,
        specification: "HTML Living Standard",
        section: "13.2.6 Tree construction",
        status: SupportStatus::Partial,
        notes: "17 of the 21 insertion modes, the list of active formatting elements \
                with its marker sites, the Noah's Ark clause, reconstruction and the \
                full adoption agency algorithm are implemented, and the spec's own \
                three worked examples for the agency are tests. Missing: `in frameset`, \
                `after frameset` and `after after frameset`, which are reachable only \
                through `<frameset>`. The `in head noscript` mode is implemented and \
                conditional on the scripting mode.",
        dependencies: &[DOM_TREE, HTML_TOKENIZER],
        tests: HTML_TREE_TESTS,
    },
    FeatureDefinition {
        id: HTML_FOREIGN,
        family: StandardFamily::Html,
        specification: "HTML Living Standard and SVG 2",
        section: "13.2.6.5 Parsing HTML fragments in foreign content",
        status: SupportStatus::Partial,
        notes: "The foreign-content dispatcher, the 44-name breakout list, all three \
                adjustment tables (37 SVG tag names, 58 SVG attributes, 11 foreign \
                attributes), the MathML text and HTML integration points and CDATA \
                sections as text nodes are implemented. Namespaced attributes are kept \
                in their own namespace and the serializer restores the prefixes. What \
                is parsed is not what is drawn: see `svg.inline-rasterization`.",
        dependencies: &[DOM_TREE, HTML_TOKENIZER, HTML_TREE],
        tests: HTML_FOREIGN_TESTS,
    },
    FeatureDefinition {
        id: HTML_TEMPLATE,
        family: StandardFamily::Html,
        specification: "HTML Living Standard",
        section: "13.2.6.4.7 The template insertion mode and 4.4.4 template contents",
        status: SupportStatus::Partial,
        notes: "A template element's contents live in a parentless DocumentFragment \
                and are never appended to the element, so the contents are inert by \
                structure rather than by a filter: they are unreachable from the \
                document and from style matching. The `content` DocumentFragment is \
                readable and writable; template contents are not cloned into a \
                rendering tree when the template is instantiated.",
        dependencies: &[DOM_TREE, HTML_TREE],
        tests: HTML_TEMPLATE_TESTS,
    },
    FeatureDefinition {
        id: HTML_SCRIPTING_MODE,
        family: StandardFamily::Html,
        specification: "HTML Living Standard",
        section: "13.2.4.5 Scripting and 15.3.1 Hidden elements",
        status: SupportStatus::Partial,
        notes: "The parser's scripting flag is a real input rather than a constant: \
                with it disabled a `noscript` element's contents parse as markup and \
                the `in head noscript` insertion mode is reachable, and with it \
                enabled they are raw text. The user-agent sheet's `noscript` display \
                rule is conditioned on the same mode, so a fallback stylesheet or \
                image is fetched and drawn for a document parsed without scripting. \
                The mode is not yet switchable at runtime and is not exposed to \
                script.",
        dependencies: &[HTML_TREE],
        tests: &[],
    },
    FeatureDefinition {
        id: HTML_FORMS,
        family: StandardFamily::Html,
        specification: "HTML Living Standard",
        section: "4.10 Form control infrastructure and 15.3.10 Form controls",
        status: SupportStatus::Partial,
        notes: "Form controls parse, are styled by the user-agent sheet, are laid out \
                as atomic inlines and carry their `value` and placeholder synthesis. \
                The form collections are implemented: `document.forms`, `form.elements`, \
                `fieldset.elements`, `form.length`, the indexed and named members of a \
                form, `control.form`, and `RadioNodeList`. Submission is implemented: \
                a trusted cancelable `submit` event, GET and urlencoded POST navigation, \
                the submitter's name/value pair, and `FormData` on the global object. \
                Not implemented: constraint validation, `reset()`, the past names map, \
                radio-group checkedness, and multipart or text/plain request bodies.",
        dependencies: &[DOM_TREE, LAYOUT],
        tests: &[],
    },
    FeatureDefinition {
        id: HTML_QUIRKS,
        family: StandardFamily::Html,
        specification: "WHATWG HTML Living Standard, Rendering",
        section: "15.3.3, 15.3.7, 15.3.8, 15.3.9 and 15.3.10 quirks-mode rules",
        status: SupportStatus::Partial,
        notes: "Detection is complete: the three-way no-quirks / limited-quirks / \
                quirks mode is computed from the doctype including `force_quirks` and \
                the missing-doctype case. Quirks mode reaches selector matching, where \
                Selectors 4 requires ASCII case-insensitive `id` and `class` matching, \
                and the user-agent sheet carries the 15.3.3 form margin, the 15.3.8 \
                table property reset, the 15.3.8 sized-nowrap-cell override, the \
                15.3.9 margin-collapsing rules and the 15.3.10 control box-sizing. \
                Not implemented: the 15.3.7 `list-style-position` rules, because that \
                property is not in the registry, and the box-model rules of the \
                original CSS 2 quirks list - unitless `line-height` inherited as a \
                number, and percentage `height`/`margin`/`padding` treated as `auto` \
                on table cells and on non-replaced inline boxes. Those are solver \
                behaviour in render-layout, and CSS 2.1 has no quirks-mode section to \
                cite them from.",
        dependencies: &[CSS_SELECTORS, HTML_TREE],
        tests: HTML_QUIRKS_TESTS,
    },
    FeatureDefinition {
        id: CSS_SYNTAX,
        family: StandardFamily::Css,
        specification: "CSS Syntax Level 3",
        section: "Tokenization, parsing and error handling",
        status: SupportStatus::Partial,
        notes: "Tokenization, declaration and at-rule parsing, and error recovery are \
                implemented, and comments are deleted during preprocessing so a \
                comment is never a combinator. Measured on 1.93 MB of production CSS: \
                0 of 11194 rules dropped, with the remaining 134 diagnostics all the \
                IE star hack that CSS Syntax 5.4.4 requires a browser to drop too. \
                The `var()` substitution stage is in `css.custom-properties`.",
        dependencies: &[],
        tests: CSS_SYNTAX_TESTS,
    },
    FeatureDefinition {
        id: CSS_SELECTORS,
        family: StandardFamily::Css,
        specification: "Selectors Level 4",
        section: "Selector syntax, matching and specificity",
        status: SupportStatus::Partial,
        notes: "Type, universal, id, class, attribute, combinator, the structural \
                pseudo-classes, `:is()`, `:where()`, `:not()`, `:has()` and the link \
                and dynamic pseudo-classes all match. The dynamic pseudo-classes read \
                `MatchContext.hovered`, `.active`, `.focused`, `.focus_visible` and \
                `.target`, seeded from `InteractionState` by \
                `Document::render_with_interaction`; the browser derives them from pointer \
                movement, primary-button press/release, click and script focus, Tab/Shift-Tab \
                sequential focus, and the committed URL fragment, so `:hover`, `:active`, \
                `:focus`, `:focus-visible`, `:focus-within` and `:target` match on a \
                rendered page. `:focus-visible` follows the browsers' rule: keyboard focus \
                always shows the ring, pointer focus only on text entry, script focus \
                inherits the last input. One limit remains: `:visited` never matches because \
                no visited-link history is exposed. Quirks-mode case-insensitive `id`/`class` \
                matching is implemented and driven by the document's mode. CSS \
                Shadow 1's `:host`, `:host()`, `:host-context()` and \
                `::slotted()` parse, match and carry their specification \
                specificity; a featureless shadow host is reachable only through \
                the host pseudo-class family.",
        dependencies: &[DOM_TREE, CSS_SYNTAX],
        tests: SELECTOR_TESTS,
    },
    FeatureDefinition {
        id: CSS_PSEUDO_ELEMENTS,
        family: StandardFamily::Css,
        specification: "CSS Pseudo-Elements Level 4",
        section: "3 Generated content and the pseudo-element syntax",
        status: SupportStatus::Missing,
        notes: "Pseudo-element syntax parses, including functional forms such as \
                `::view-transition-new/old(root)` from a real production sheet. \
                Nothing consumes it: there is no generated-content model, so \
                `::before` and `::after` produce nothing and `content` has no \
                implementation. The `q::before { content: open-quote }` rule is \
                deliberately absent from the user-agent sheet rather than present and \
                inert.",
        dependencies: &[CSS_SYNTAX],
        tests: &[],
    },
    FeatureDefinition {
        id: CSS_CASCADE,
        family: StandardFamily::Css,
        specification: "CSS Cascading and Inheritance Level 6",
        section: "Cascade sorting order, defaulting, and the @scope rule",
        status: SupportStatus::Partial,
        notes: "Origin and importance sorting, specificity, `@layer` with \
                `revert-layer`, the CSS-wide keywords including `revert`, custom \
                property substitution and the shorthand expansions `background`, \
                `margin`/`padding`, `border*`, `font`, `text-decoration` and the \
                legacy `grid-gap` longhands are implemented. `@scope` is evaluated \
                with explicit and implicit roots, scoping limits, relative preludes, \
                nested scopes, scoped declarations and scope proximity between \
                specificity and order of appearance; `@import ... scope(...)` is the \
                remaining scope surface. Author sheets are scoped to the node \
                tree they belong to, with the Context sort step ranking normal \
                outer-tree declarations above inner-tree ones and important \
                inner-tree declarations above outer ones. One documented origin \
                error remains: HTML presentational hints cascade at the user-agent \
                origin rather than the author origin the standard specifies, so the \
                user-agent sheet has to declare `td, th` and `table` through `:where` \
                to avoid outranking `cellpadding` and `cellspacing`.",
        dependencies: &[CSS_SYNTAX, CSS_SELECTORS],
        tests: CASCADE_TESTS,
    },
    FeatureDefinition {
        id: CSS_NESTING,
        family: StandardFamily::Css,
        specification: "CSS Nesting Module Level 1",
        section: "2 Syntax, 3 Relative selectors, 5 Adapting existing syntax",
        status: SupportStatus::Partial,
        notes: "`&` desugars to `:is(parent)` at the token level, so nesting gets both \
                matching and specificity from existing machinery instead of the \
                cross-product blowup the specification warns about, and the \
                declaration-versus-rule test of 5.5.5 is implemented so \
                `margin: calc(50% - 10px) auto` stays a declaration. Measured impact \
                on the production corpus: 0 rules, because the `&` characters in it \
                are all inside `url(...)` query strings - a zero confirmed against a \
                corpus-aware tool rather than a blind spot.",
        dependencies: &[CSS_SYNTAX, CSS_CASCADE],
        tests: &[],
    },
    FeatureDefinition {
        id: CSS_VARIABLES,
        family: StandardFamily::Css,
        specification: "CSS Custom Properties Level 1",
        section: "Computed-value substitution and cycles",
        status: SupportStatus::Partial,
        notes: "Substitution, fallbacks, cycles, inheritance and the guarantee that a \
                custom property's value is a token stream are implemented. Typed \
                registration of custom properties through `@property` is not.",
        dependencies: &[CSS_CASCADE],
        tests: VARIABLE_TESTS,
    },
    FeatureDefinition {
        id: CSS_TYPED_VALUES,
        family: StandardFamily::Css,
        specification: "CSS Values and Units Level 4",
        section: "Property grammars and computed values",
        status: SupportStatus::Partial,
        notes: "The property grammar machinery is there and the registry carries typed \
                computed values for the properties that have consumers. The gap is \
                coverage, and it is the load-bearing one: because the registry installs \
                an initial value for every registered property, `ComputedStyle::get` \
                returns `Some` for all of them, so 'the author declared this' and 'this \
                is the initial value' are indistinguishable. Any solver logic that needs \
                author intent is silently wrong as a result.",
        dependencies: &[CSS_VARIABLES],
        tests: TYPED_VALUE_TESTS,
    },
    FeatureDefinition {
        id: CSS_COLORS,
        family: StandardFamily::Css,
        specification: "CSS Color Level 4",
        section: "4 RGB and HSL, 10 HWB, 12 Lab and LCH, 13 OKLab and OKLCH, 14 color(), 15 color-mix()",
        status: SupportStatus::Partial,
        notes: "Named colours, all the rgb() and hsl() syntaxes, `hwb()`, `lab()`/`lch()`, \
                `oklab()`/`oklch()`, `color()` in every predefined space, and \
                `color-mix()` in those spaces with premultiplied alpha and all four \
                hue paths are implemented. Missing (`none`) components parse in every \
                modern function, carry forward to analogous components during \
                interpolation, and behave as zero elsewhere. Out-of-gamut results use \
                the §14.2.2 Binary Search + Local MINDE mapping at the sRGB device \
                boundary rather than per-channel clipping. Relative color syntax is \
                implemented for rgb(), hsl(), hwb(), lab(), lch(), oklab(), oklch() \
                and color(), including channel keywords, `none`, alpha inheritance \
                and basic `calc()` arithmetic. `alpha()` changes only the origin's \
                alpha while retaining its components. `light-dark()` selects its \
                arm from the element's used color scheme. `color-scheme` is a typed \
                inherited property: `normal` follows the page scheme, an explicit \
                `light`/`dark`/list resolves per element, and the used scheme drives \
                `light-dark()` and the `Canvas`/`CanvasText` system colors through \
                `SystemPalette::for_scheme`. It does not yet change the page-level \
                `prefers-color-scheme` media query, and `device-cmyk()` and \
                `contrast-color()` remain absent.",
        dependencies: &[CSS_TYPED_VALUES],
        tests: TYPED_VALUE_TESTS,
    },
    FeatureDefinition {
        id: CSS_USED_VALUES,
        family: StandardFamily::Css,
        specification: "CSS Values and Units Level 4",
        section: "Used values and math function resolution",
        status: SupportStatus::Partial,
        notes: "`calc()`, `min()`, `max()` and `clamp()` resolve, and length units \
                including font-relative and absolute units resolve against a length \
                context. Viewport units resolve against the layout viewport. The used \
                value of `vertical-align` as a percentage of line-height is not \
                implemented, and no property has a used-value stage beyond the ones the \
                layout solver and paint layer read.",
        dependencies: &[CSS_TYPED_VALUES],
        tests: TYPED_VALUE_TESTS,
    },
    FeatureDefinition {
        id: CSS_MEDIA,
        family: StandardFamily::Css,
        specification: "Media Queries Level 5",
        section: "5 Evaluating media features and 4 Media queries",
        status: SupportStatus::Partial,
        notes: "Media types and the `width`, `min-width`, `max-width`, `height`, \
                `min-height`, `max-height` and `orientation` features evaluate, \
                including the `(min-width:1560px)and (max-width:2059.9px)` form real \
                sheets write with no spaces around `and`. There is exactly one \
                evaluator: the support question a consumer asks is answered by the same \
                tri-state evaluation the matcher uses, so a query cannot be evaluated \
                one way and described another. Not evaluated: `resolution` and the \
                `*-device-pixel-ratio` family, which need a device pixel ratio in the \
                match context; `hover`, `pointer` and the `prefers-*` features; and the \
                range context syntax `(400px <= width <= 700px)`.",
        dependencies: &[CSS_SELECTORS, CSS_CASCADE],
        tests: &[],
    },
    FeatureDefinition {
        id: CSS_FONT_FACE,
        family: StandardFamily::Css,
        specification: "CSS Fonts Level 4",
        section: "5 @font-face",
        status: SupportStatus::Partial,
        notes: "A `@font-face` block's descriptors are parsed and resolved (§4.1 \
                requires a `font-family` and a `src` and a rule without either is not \
                considered, with a diagnostic), §4.2/§4.4's `font-family`, \
                `font-weight` and `font-style` become the matcher's axes, §4.5's \
                `unicode-range` narrows the effective character map and orders a \
                family's rules last-declared-first (§4.5.1), §4.3.3 skips a `src` \
                item whose format or technology this engine cannot use *before* \
                requesting it, and §4.3.3.1's `local()` is resolved against \
                installed face names ahead of any URL. A fetched body is decoded and \
                registered, the faces are in the *same* table §5 searches, and §5.2's \
                shadowing is implemented, so a document family replaces an installed \
                family of the same name even when its faces have not arrived. The \
                faces are document-scoped and dropped with the document; a change of \
                table mints new `FontInstanceId`s, so a memo entry or a rasterised \
                mask can never outlive the table it was made against. A face that \
                has not arrived is treated as not present in its family - §5.2's own \
                rule - and because that is the whole of the state, measurement and \
                painting cannot disagree about it. `font-display` now has a document \
                timeline: auto/block use a 3s block and infinite swap, swap uses no \
                block and infinite swap, fallback accepts arrival before 3.1s, and \
                optional before 100ms; an arrival at the failure boundary is too late. \
                The block period deliberately measures/paints the visible fallback \
                because the engine has one measure/paint table and §4.8.1 requires \
                visible text. Author stylesheet discovery passes every eligible \
                `<style>` and fetched `<link>` sheet's rules to a pre-layout installer \
                after evaluating their `@media`/`@supports` conditions; non-matching \
                faces are never planned, fetched or installed. The installer resolves \
                §4.3.2 URLs and publishes the document table before the first \
                measurement. The browser plans the HTTP(S) URLs §4.8.1 requires \
                (`DocumentFonts::plan_fetches` narrowed by computed families and page \
                characters), submits them through its cached network batch, and decodes \
                the returned bytes - raw sfnt, WOFF 1.0 zlib, or WOFF2 Brotli - before \
                the next render. `file:` and base64 `data:` resources load synchronously. \
                NOT implemented: §4.6's feature and variation settings, §4.7's \
                `font-named-instance`, §4.11's `*-override` metrics descriptors, any \
                font technology (a `tech()` item is skipped), Embedded OpenType and SVG \
                fonts, WOFF2 files whose `hmtx` transform the decoder does not support, \
                and `@container`-gated `@font-face`.",
        dependencies: &[CSS_SYNTAX],
        tests: &[],
    },
    FeatureDefinition {
        id: CSS_ANIMATIONS,
        family: StandardFamily::Css,
        specification: "CSS Animations Level 1 and Web Animations Level 1",
        section: "@keyframes and the Animation interface",
        status: SupportStatus::Partial,
        notes: "`@keyframes` blocks and the `animation` shorthand are parsed and \
                sampled by a time-parameterised style pass; opacity, colour, \
                transform, lengths and other computed properties are interpolated. \
                The browser drives a 16 ms resampling clock while an animation is \
                active and stops it after finite animations. Comma-separated animation \
                lists are sampled in order and `transform: none` interpolates from the \
                identity. A self-hosted Web Animations core supplies element.animate, \
                play/pause/finish/cancel, finish/cancel events and a frame-driven \
                timeline, but it is not yet unified with the CSS animation timeline and \
                lacks playbackRate/reverse, effect composition and per-keyframe easing. \
                Scroll composition runs on a dedicated compositor worker, but animation \
                sampling still runs on the event-loop thread and is not driven by a \
                compositor timeline.",
        dependencies: &[CSS_SYNTAX],
        tests: &[ConformanceTest::new("rust", "css::animation::tests")],
    },
    FeatureDefinition {
        id: CSS_TRANSITIONS,
        family: StandardFamily::Css,
        specification: "CSS Transitions Level 2",
        section: "3.3 Defining before-change style: the @starting-style rule",
        status: SupportStatus::Partial,
        notes: "The `transition` shorthand and its four longhands parse and reach \
                computed style. A per-document sampler remembers the previous computed \
                value, starts a transition when a covered property changes, interpolates \
                numbers, lengths, transform functions and sRGB colours, honours duration, \
                delay and the shipped timing functions, and reports transitionend. \
                Interrupted reversals use the reversing shortening factor, and \
                `transform: none` interpolates from the identity. \
                `transition-behavior: allow-discrete` steps discrete properties and \
                transitionrun/transitionstart/transitionend/transitioncancel are \
                reported for style changes, node removal and document switches. \
                Reversing-adjusted start values follow CSS Transitions 1 §3.1. \
                `@starting-style` rules parse at top level and nested in a style rule, \
                cascade in their ordinary source position for the starting style, and \
                supply the origin of entry transitions for elements that were not \
                rendered at the previous style change event (first render, DOM \
                insertion, and display:none to rendered). The self-hosted Web \
                Animations core covers element.animate, playbackRate/reverse and \
                composition on the same document clock as this transition timeline.",
        dependencies: &[CSS_SYNTAX],
        tests: &[ConformanceTest::new("rust", "css::transition::tests")],
    },
    FeatureDefinition {
        id: CSS_TEXT_DECORATION,
        family: StandardFamily::Css,
        specification: "CSS Text Decoration Level 3",
        section: "3 Line decorations",
        status: SupportStatus::Partial,
        notes: "The `text-decoration` shorthand expands, so an author's \
                `text-decoration: none` beats the user-agent sheet's \
                `text-decoration-line: underline` on a link. Paint produces \
                `text-decoration-line`, `-style` and `-color` with underline, \
                overline, line-through and blink, and the paint layer walks formatting \
                ancestors to find the first decoration specifying a line. That walk is \
                an approximation of the CSS Decoration propagation algorithm: an \
                intermediate inline that sets `text-decoration-line: none` cannot switch \
                the decoration back off, and moving propagation into the cascade is the \
                documented fix.",
        dependencies: &[CSS_CASCADE, PAINT],
        tests: &[],
    },
    FeatureDefinition {
        id: CSS_FLEXBOX,
        family: StandardFamily::Rendering,
        specification: "CSS Flexible Box Layout Module Level 1",
        section: "Single-line flex containers and flexible lengths",
        status: SupportStatus::Partial,
        notes: "Single-line flex: `flex-direction: row` and `column`, `flex-basis`, \
                `flex-grow`, `flex-shrink`, the resolved `flex` shorthand, intrinsic \
                sizing of flex items, and `align-items` / `justify-content` along the \
                main and cross axes are implemented. Wrapping (`flex-wrap`), \
                `order`, and the automatic minimum size rules of 9.9 are not.",
        dependencies: &[CSS_TYPED_VALUES],
        tests: FLEXBOX_TESTS,
    },
    FeatureDefinition {
        id: CSS_GRID,
        family: StandardFamily::Rendering,
        specification: "CSS Grid Layout Module Level 1",
        section: "Explicit tracks, track sizing, gaps, and auto-placement",
        status: SupportStatus::Partial,
        notes: "Explicit track lists including `fr`, `repeat()` with fixed and \
                `auto-fill` / `auto-fit` repetition, line names, `auto-placement` by \
                sparse packing, and the row and column gaps are implemented. The \
                implicit grid algorithm of 7.2, `grid-auto-flow: dense`, named line \
                placement from `grid-column` / `grid-row` shorthand, and alignment of \
                the grid container are not.",
        dependencies: &[CSS_TYPED_VALUES],
        tests: GRID_TESTS,
    },
    FeatureDefinition {
        id: LAYOUT,
        family: StandardFamily::Rendering,
        specification: "CSS Display and CSS Box specifications",
        section: "Formatting structure and layout",
        status: SupportStatus::Partial,
        notes: "Block, inline, inline-block, list-item and flex formatting, floats, \
                absolute, relative and sticky positioning, multi-column layout with \
                `column-count`, `column-width`, `column-gap`, `column-fill` and \
                `column-span: all`, CSS 2.1 §8.3 vertical margin collapsing \
                (adjoining siblings, parent-first/last, empty-block \
                collapse-through, positive and negative margins), the intrinsic and min/max sizing \
                passes and text shaping through a measurer seam are implemented. A sticky \
                box is displaced when it is painted and hit tested, against the page's \
                scroll offset; a sticky box inside a nested scroll container uses that \
                container's offset as zero, because nested containers have no offset yet. \
                CSS Writing Modes 3 `direction` and UAX #9 line reordering are implemented \
                for horizontal writing modes: paragraph level, visual level runs, odd-run \
                reversal, start/end alignment, RTL text-indent, and the HTML `dir` attribute. \
                Horizontal-tb RTL now also mirrors block placement, over-constrained/auto \
                margins, root direction, floats/clear, relative/absolute inset preference, \
                flex row/row-reverse/justify-content/cross-axis/wrapping, grid logical \
                columns and table column/spanning-cell order. CSS logical box properties \
                (`margin/padding/inset/border-{inline,block}-{start,end}-*` and their \
                shorthands) resolve to physical sides with full cascade competition. \
                `text-indent`, `letter-spacing` and `word-spacing` affect measurement, line \
                breaking, intrinsic widths, painting and hit-testing. \
                `content-visibility: hidden` keeps the element's box, applies size, layout \
                and paint containment, and omits every descendant fragment; `content-visibility: \
                auto` currently lays its contents out normally, without off-screen skipping \
                or `contain-intrinsic-size`. \
                Not implemented: `column-rule`, `unicode-bidi` embeddings and isolates, \
                per-inline-element `direction`, UAX #9 L3/L4 (combining marks and mirroring), \
                Arabic/Indic shaping, `dir=auto`, vertical writing modes, logical sizing/corner/\
                scroll properties, flex/grid `justify-items`/`justify-self`, table `<col>` \
                direction, grapheme-cluster text spacing, and scrolling a nested scroll \
                container. Each one has a scrollport geometry in the \
                fragment tree, but only the page viewport owns a scroll offset, so \
                `overflow: auto` on a `div` clips its content with no way to reach it.",
        dependencies: &[DOM_TREE, CSS_USED_VALUES, CSS_FLEXBOX, CSS_GRID],
        tests: LAYOUT_TESTS,
    },
    FeatureDefinition {
        id: CSS_TABLES,
        family: StandardFamily::Rendering,
        specification: "CSS 2.1",
        section: "17 Tables",
        status: SupportStatus::Partial,
        notes: "Section 17 is implemented: anonymous table boxes, 17.5.2 column widths, \
                17.5.3 row heights, cell and row-group `vertical-align`, colspan and \
                rowspan, the 17.4 caption, 17.5.1 `<col>` and `<colgroup>` widths, \
                17.5.2.1 `table-layout: fixed`, 17.6 `border-collapse` with conflict \
                resolution and the `hidden` override, and 17.5.1.1 `empty-cells`. One \
                recorded deviation: 17.6.2 splits a collapsed border at the grid line, \
                half to each side, and this implementation gives the whole resolved \
                border to the winning side, because the rasteriser resolves border \
                colour from the element's computed style rather than from the fragment \
                and a half-and-half split would paint two colours along one edge. The \
                occupied space stays correct, which is what column widths and content \
                offsets depend on.",
        dependencies: &[DOM_TREE, CSS_USED_VALUES, LAYOUT],
        tests: TABLE_TESTS,
    },
    FeatureDefinition {
        id: SVG_INLINE,
        family: StandardFamily::Rendering,
        specification: "SVG 2",
        section: "3.2.1 Rendered and non-rendered elements, 3.11 Overflow, \
                   7 Shapes, 8 Paths and 4.2 Presentation attributes",
        status: SupportStatus::Partial,
        notes: "An inline `<svg>` element is detected in the SVG namespace and its \
                subtree is serialised with the prefixes restored. The document pipeline \
                rasterises it after the cascade with the same rasteriser that draws an \
                `<img src=icon.svg>`, layers the raster over the page's images, and paints \
                it through the ordinary image command into the element's laid-out box. Its \
                size comes from the `width`/`height` geometry attributes with a `viewBox` \
                fallback. `currentColor` resolves to the element's computed `color`, and a \
                root `fill` or `stroke` attribute is inherited by its shapes. The \
                never-rendered element types take no part in layout. The rasteriser's \
                subset is shapes, `path` with `M m L l H h V v C c S s Q q T t Z z` where \
                `A` degrades to a line, `<g>` transforms and `viewBox` sizing; a stroke is \
                one device pixel wide. Not implemented: author CSS on the svg's children, \
                because `fill` and `stroke` are not implemented properties, so a shape \
                styled only by a stylesheet is filled black where a browser would use the \
                stylesheet's fill. \
                Not implemented: `use` and `symbol` indirection, which SVG 2 3.2.4 \
                renders as a shadow tree cloned from its target; gradients; `text`; \
                `clipPath`; `mask`; `<image>`; and `foreignObject` positioning inside \
                the SVG viewport coordinate system. SVG filter chains are implemented \
                for `feGaussianBlur`, `feColorMatrix`, `feOffset`, `feBlend`, \
                `feComposite`, `feFlood`, `feMerge`, `feDropShadow` and `feMorphology`, \
                with in/in2/result, units, subregions and region clipping; turbulence, \
                displacement, image, component-transfer, convolve, tile, lighting, \
                linearRGB interpolation and fractional offsets remain open. An `<svg>` \
                with a percentage or absent `width` is sized from its `viewBox` where a \
                browser would use the containing block, which is the safe direction and \
                is documented as a difference.",
        dependencies: &[HTML_FOREIGN, LAYOUT, PAINT],
        tests: SVG_TESTS,
    },
    FeatureDefinition {
        id: PAINT,
        family: StandardFamily::Rendering,
        specification: "CSS 2 and CSS Painting specifications",
        section: "Painting order, stacking contexts and visual effects",
        status: SupportStatus::Partial,
        notes: "The display list and a CPU rasteriser implement backgrounds and \
                gradients, solid, gradient and image content, borders including the \
                collapsed-model ring, text glyph runs, text decoration, text shadow, \
                list markers, clipping by `overflow` and `clip-path`-adjacent rect \
                clips, and stacking contexts from `opacity` and `transform`. The \
                retained compositor caches root segments, top-level \
                translate/opacity/scale-rotate-skew layers, fixed viewport-space \
                content, top-level sticky scopes, and top-level `filter`/`mask` layers \
                as single effect surfaces so blur can cross band boundaries; scrolling \
                composites those bands, resolves sticky displacement at composite \
                time, and only rasterizes newly exposed bands. A dedicated compositor \
                worker runs scroll jobs off the render worker and presents through a \
                platform sink directly; on Windows that sink is a wgpu/D3D12 swapchain \
                that uploads the composed frame as a texture and presents through the \
                GPU, with softbuffer as the fallback. `backdrop-filter`, `mix-blend-mode` \
                and `clip-path` as an arbitrary shape still lack consumers; non-normal \
                blend modes, ambient clips crossing a layer, nested sticky and effects \
                above the effect-surface size ceiling still take the complete raster. \
                `z-index` sorts block siblings only: flex and grid children are not \
                sorted and the paint layer has no other notion of stacking level.",
        dependencies: &[LAYOUT],
        tests: PAINT_TESTS,
    },
    FeatureDefinition {
        id: JS,
        family: StandardFamily::EcmaScript,
        specification: "ECMAScript Language Specification",
        section: "Execution contexts, objects and jobs",
        status: SupportStatus::Partial,
        notes: "The tree-walking interpreter runs whole production bundles to \
                completion, including 2.27 MB of transpiled application code through \
                the class, `Proxy`/`Reflect`, iterators and Web Storage layers, and \
                `ToObject` primitive boxing is implemented so every `Object.*` static \
                accepts a primitive. Not conformant: the test262 gate runs as a \
                regression floor at roughly a third of the suite passing, and the \
                platform surface is missing globals that throw a `ReferenceError` \
                and take the surrounding script with them. Known remaining defects \
                are recorded per-item in `docs/visual_fidelity_gaps.md` rather than \
                left to be discovered.",
        dependencies: &[],
        tests: JS_TESTS,
    },
    FeatureDefinition {
        id: EVENT_LOOP,
        family: StandardFamily::Html,
        specification: "HTML Living Standard",
        section: "8.1.7 Event loops",
        status: SupportStatus::Partial,
        notes: "Tasks, microtasks and the rendering opportunity, timer coalescing and \
                the network integration point are implemented, with bounded queueing \
                that parks rather than blocking the loop. Not implemented: worker \
                event loops, `MessagePort` and `postMessage`.",
        dependencies: &[DOM_TREE, JS],
        tests: EVENT_LOOP_TESTS,
    },
    FeatureDefinition {
        id: URL_PARSER,
        family: StandardFamily::Url,
        specification: "WHATWG URL Standard",
        section: "URL parsing, serialization and relative resolution",
        status: SupportStatus::Partial,
        notes: "The parser, the host parser including IPv4 and IPv6, percent-encoding \
                sets, the special-scheme rules and relative resolution are \
                implemented. IDNA is applied through a UTS 46 table; the full \
                preprocessing and contextual rules for a non-ASCII domain are not \
                independently verified.",
        dependencies: &[],
        tests: URL_TESTS,
    },
    FeatureDefinition {
        id: NAVIGATION_HISTORY,
        family: StandardFamily::Html,
        specification: "HTML Living Standard",
        section: "7.4 Navigation and 7.2.6 Session history traversal",
        status: SupportStatus::Partial,
        notes: "Same-document and cross-document navigation and the session history \
                are implemented. `history.pushState` and `replaceState` change the URL \
                and `history.state` without a load. `back`, `forward` and `go` reach an \
                entry of the same document without a load and fire `popstate` with its \
                state; an entry of another document is loaded. Not implemented: restoring \
                `history.state` when a traversal loads a document, and `scrollRestoration`. \
                `history.length` reflects the list as of each turn's start. An `iframe` has \
                its own document and realm (`contentWindow`, `parent`, `top`, `postMessage`), \
                but it is not painted, and `window.open` and `target=_blank` do nothing.",
        dependencies: &[URL_PARSER],
        tests: NAVIGATION_TESTS,
    },
    FeatureDefinition {
        id: FETCH,
        family: StandardFamily::Fetch,
        specification: "Fetch Standard",
        section: "Fetching, CORS and HTTP-network fetch",
        status: SupportStatus::Partial,
        notes: "`fetch` and `XMLHttpRequest` are implemented over a pooled transport \
                that reuses connections, sets `.encoding` rather than relying on the \
                brotli reader draining the length-delimited body, and reports a \
                terminal outcome naming its phase and elapsed time for every request. \
                CORS covers simple and preflighted requests with no credentials, \
                checked on the final response of a redirect chain rather than at each \
                hop. Not implemented: credentialed requests, `no-cors` semantics, \
                preflight caching, streaming request and response bodies. HTTP/2 is \
                rejected rather \
                than absent: a pooled connection is removed from the pool while in use \
                and `run` holds it for the whole request, so one connection carries one \
                in-flight request, which is the exact inverse of multiplexing.",
        dependencies: &[EVENT_LOOP, URL_PARSER],
        tests: &[],
    },
    FeatureDefinition {
        id: VIDEO_DECODE,
        family: StandardFamily::Infra,
        specification: "ISO/IEC 14496-12 and ISO/IEC 14496-10",
        section: "Container sample descriptions, avcC and the H.264 bitstream",
        status: SupportStatus::Partial,
        notes: "The container and bitstream layers are done: MP4 sample description \
                parsing, `avcC` record parsing, NAL unit classification, SPS parsing \
                for the coded picture size, emulation-prevention byte stripping and \
                Annex-B conversion. Pixel decode is not: entropy decoding and \
                reconstruction sit behind a `VideoDecoder` trait and the shipped \
                default is a placeholder that always reports \
                `DecoderUnavailable`, so no site ever presents a decoded frame and a \
                `<video>` shows its poster and nothing else. Decoding is not a \
                self-contained deliverable: it needs `render-net` to fetch the media \
                and a decoder, and neither is in this registry.",
        dependencies: &[JS],
        tests: VIDEO_TESTS,
    },
];
