//! A revision-preserving HTML-to-pixels pipeline.
//!
//! Parsing and rendering are deliberately separate. Script bindings mutate the
//! [`Dom`] owned by [`Document`], then render that same tree at its new
//! revision; ordinary DOM updates never require reparsing the HTML source.

#![allow(clippy::too_many_lines)]

use std::collections::{BTreeMap, HashMap};

use url::Url;

use crate::css::cascade::{
    CascadeInput, CascadeOrigin, media_query_list_is_supported, media_query_list_matches,
};
use crate::css::computed::{
    ComputationDiagnostic, ComputationLimits, ComputedStyle, PropertyRegistry,
    compute_document_styles_with_hints,
};
use crate::css::selector::MatchContext;
use crate::css::stylesheet::{Declaration, StyleSheet, StyleSheetDiagnostic, parse_stylesheet};
use crate::dom::{Dom, DomRevision, ElementData, Namespace, Node, NodeId, NodeKind};
use crate::html::{HtmlParseError, QuirksMode, parse_document_with_scripting};
use crate::image::ImageResources;
use crate::image::inline_svg::svg_geometry;
use crate::layout::{
    FormattingDiagnostic, FormattingLimits, FormattingTree, LayoutDiagnostic, LayoutOptions,
    LayoutOutput, PhysicalPoint, SimpleTextMeasurer, TextMeasurer, build_formatting_tree,
    layout_formatting_tree_with_images,
};
use crate::paint::{
    Color, CpuRasterOutput, CpuRasterizer, DisplayListBuildOutput, DisplayListBuilderOptions,
    DisplayListDiagnostic, GlyphMaskProvider, NoGlyphMasks, RasterDiagnostic, ReferenceTextShaper,
    TextShaper, build_display_list_with_images,
};

/// The HTML user-agent style sheet.
///
/// Source: WHATWG HTML Standard, "Rendering" chapter
/// (<https://html.spec.whatwg.org/multipage/rendering.html>): §15.2 states
/// that "the CSS rules given in these subsections are ... expected to be used
/// as part of the user-agent level style sheet defaults for all documents that
/// contain HTML elements", and §15.3.1-§15.3.12 plus §15.5.4-§15.5.6 give the
/// per-element suggestions this sheet follows. Presentational *attribute*
/// mappings from the same chapter stay in [`presentational_hint_declarations`]
/// below, which cascades them per element at the same origin.
///
/// Longhands are intentional: shorthand expansion is a separate CSS feature.
///
/// Adaptations to this engine, each of which is a capability gap rather than a
/// preference:
///
/// * The spec writes logical properties (`margin-block`,
///   `padding-inline-start`, `inset-inline-start`, `border-inline-width`, ...).
///   The layout solver consumes physical longhands only, so every logical
///   property below is written out physically. This sheet is therefore
///   left-to-right; the block direction and RTL mirror need `direction` support
///   in the layout solver (render-layout) first.
/// * `font-weight`, `font-style` and `font-family` reach layout and paint through
///   the layout solver's `TextStyle` and the text fragment it is measured into,
///   so a heading renders at its own weight, emphasis at its own slant, and
///   `pre`/`code` in a monospace face. What is still missing is everything
///   `font-synthesis` and `@font-face` would add on top; see
///   `docs/visual_fidelity_gaps.md` S1.
/// * `small-caps`, `text-transform`, `letter-spacing`, `text-indent` and
///   `vertical-align` reach the computed style. `text-transform`,
///   `letter-spacing`, `text-indent` and `vertical-align` have consumers;
///   `small-caps` does not yet.
/// * `q` has no rule here on purpose: §15.3.4 styles it through
///   `q::before { content: open-quote }` and `q::after { content: close-quote }`,
///   and this engine does not generate content. Substituting a font change
///   would be a different specification, not a smaller one.
/// * Quirks-mode rules (§15.3.9 margin collapsing quirks, the `li` inside
///   `list-style-position` default, and the table font reset) are not applied;
///   this sheet is the no-quirks rendering.
/// * `dialog` uses physical `left`/`right` instead of the spec's logical
///   `inset-inline-*` pairs for the same reason as above.
const UA_STYLE_SHEET: &str = r#"
/* §15.3.1 Hidden elements. */
head, area, base, basefont, datalist, link, meta, noembed, noframes, param,
rp, script, style, template, title, track, [hidden] { display: none; }
input[type="hidden" i] { display: none; }

/* §15.3.2 The page. */
html, body { display: block; }
body { margin-top: 8px; margin-right: 8px; margin-bottom: 8px; margin-left: 8px; }

/* §15.3.3 Flow content. The block margins of the elements with default
   margins are declared through `:where` for the reason given at
   [`QUIRKS_STYLE_SHEET`]: the quirks-mode margin-collapsing rules of §15.3.9
   are user-agent rules too, and a type selector would outrank them. */
html, body, address, article, aside, blockquote, center, details, dialog, div,
dd, dl, dt, fieldset, figcaption, figure, footer, form, header, hgroup, hr,
legend, listing, main, menu, nav, ol, p, plaintext, pre, search, section, ul,
xmp, h1, h2, h3, h4, h5, h6 { display: block; }
center { text-align: center; }
:where(blockquote, listing, p, plaintext, pre, xmp) {
  margin-top: 1em; margin-bottom: 1em;
}
blockquote, figure { margin-left: 40px; margin-right: 40px; }
address { font-style: italic; }
listing, plaintext, pre, xmp { font-family: monospace; white-space: pre; }
dialog:not([open]) { display: none; }
dialog {
  position: absolute;
  left: 0; right: 0;
  padding: 1em;
  border: 1px solid;
  background-color: canvas;
  color: canvastext;
}

/* §15.3.4 Phrasing content. */
cite, dfn, em, i, var { font-style: italic; }
b, strong { font-weight: bolder; }
code, kbd, samp, tt { font-family: monospace; }
big { font-size: larger; }
small { font-size: smaller; }
sub { vertical-align: sub; }
sup { vertical-align: super; }
sub, sup { line-height: normal; font-size: smaller; }
ruby { display: ruby; }
rb { display: ruby-base; }
rtc { display: ruby-text-container; }
rt { display: ruby-text; }
a:link { color: #0000ee; text-decoration-line: underline; }
a:visited { color: #551a8b; text-decoration-line: underline; }
mark { background-color: yellow; color: black; }
abbr[title], acronym[title] {
  text-decoration-line: underline;
  text-decoration-style: dotted;
}
ins, u { text-decoration-line: underline; }
del, s, strike { text-decoration-line: line-through; }
nobr { white-space: nowrap; }

/* §15.3.6 Sections and headings. */
article, aside, hgroup, nav, section { display: block; }
h1, h2, h3, h4, h5, h6 { font-weight: bold; }
h1 { font-size: 2em; }
h2 { font-size: 1.5em; }
h3 { font-size: 1.17em; }
h4 { font-size: 1em; }
h5 { font-size: 0.83em; }
h6 { font-size: 0.67em; }
:where(h1) { margin-top: 0.67em; margin-bottom: 0.67em; }
:where(h2) { margin-top: 0.83em; margin-bottom: 0.83em; }
:where(h3) { margin-top: 1em; margin-bottom: 1em; }
:where(h4) { margin-top: 1.33em; margin-bottom: 1.33em; }
:where(h5) { margin-top: 1.67em; margin-bottom: 1.67em; }
:where(h6) { margin-top: 2.33em; margin-bottom: 2.33em; }

/* §15.3.7 Lists. */
dir, dd, dl, dt, menu, ol, ul { display: block; }
li { display: list-item; }
:where(dir, dl, menu, ol, ul) { margin-top: 1em; margin-bottom: 1em; }
:where(dl menu, dl ol, dl ul, ol ol, ol ul, ul menu, ul ol, ul ul) {
  margin-top: 0; margin-bottom: 0;
}
dd { margin-left: 40px; }
dir, menu, ol, ul { padding-left: 40px; }
ol { list-style-type: decimal; }
dir, menu, ul { list-style-type: disc; }
ul ul, ul menu, ol ul, ol menu, menu ul, menu menu {
  list-style-type: circle;
}
ul ul ul, ul ul menu, ul menu ul, ul menu menu,
ol ul ul, ol ul menu, ol menu ul, ol menu menu,
menu ul ul, menu ul menu, menu menu ul, menu menu menu {
  list-style-type: square;
}

/* §15.3.8 Tables. */
table { display: table; }
caption { display: table-caption; }
colgroup { display: table-column-group; }
col { display: table-column; }
thead { display: table-header-group; }
tbody { display: table-row-group; }
tfoot { display: table-footer-group; }
tr { display: table-row; }
td, th { display: table-cell; }
th { font-weight: bold; }
caption { text-align: center; }
/* The engine cascades presentational attributes at the user-agent origin with
   zero specificity (HTML5 §15.3) instead of the author origin the standard
   specifies (§15.2), so a UA rule only yields to `cellpadding`/`cellspacing`
   when it too has zero specificity. These two declarations therefore use
   `:where`; moving the hints to their specified origin removes the need. */
:where(td, th) { padding: 1px; }
:where(table) { box-sizing: border-box; border-spacing: 2px; }
/* §15.3.8, in the "presentational hints" block: a cell with a `nowrap`
   attribute does not wrap. Declared through `:where` so that the quirks-mode
   override in `presentational_hint_declarations` - which the specification
   says must override this rule, and which sits at the same zero specificity -
   wins on order. */
:where(td[nowrap], th[nowrap]) { white-space: nowrap; }

/* §15.3.10 Form controls. The spec defers the widget look to §15.5 and no
   longer states a control font; engines ship a smaller UI face than the
   document face, and `font-size` is the half of that this engine renders. */
button, input, select, textarea { font-size: 13.3333px; }
button, input, select, textarea { display: inline-block; }
/* A single-line input and a button centre their text vertically in the box
   (HTML §15.5). The value is the input's only child, so a centred flex box
   places it without changing any other content. */
input:not([type="hidden" i]) { display: inline-flex; align-items: center; }
button, input:is([type="reset" i], [type="button" i], [type="submit" i]) {
  text-align: center;
  justify-content: center;
}
button, input, select, textarea { box-sizing: border-box; }
textarea { white-space: pre-wrap; }
input:not([type="hidden" i]) {
  width: 180px; min-height: 22px;
  padding-left: 4px; padding-right: 4px;
  border: 1px solid #888;
}

/* §15.3.11 The hr element. */
hr {
  color: gray;
  border-style: inset;
  border-width: 1px;
  margin-top: 0.5em; margin-bottom: 0.5em;
  margin-left: auto; margin-right: auto;
  overflow: hidden;
}

/* §15.3.12 The fieldset and legend elements. `ThreeDFace` is a system colour
   this engine does not define, so the system button face is spelled out. */
fieldset {
  border: 2px groove;
  border-color: #c0c0c0;
  padding-top: 0.35em; padding-bottom: 0.625em;
  padding-left: 0.75em; padding-right: 0.75em;
}
legend { padding-left: 2px; padding-right: 2px; }

/* §15.5.5 The details and summary elements. */
details > summary:first-of-type { display: list-item; }

/* Inline SVG (SVG 2 §3.2.1, §3.11 and §4.2).

   `width` and `height` on an `svg` element are presentation attributes for the
   CSS properties of the same name (SVG 2 §4.2), so the element is sized by its
   own geometry attributes rather than by its contents; the presentational-hint
   path below supplies them, falling back to the `viewBox` extent.

   `display: inline-block` is this engine's choice and not a quotation. The
   initial value of `display` is `inline`, and a character-level inline box has
   no geometry, so with the initial value every inline icon would have no box
   to paint into. `inline-block` is the closest value this engine has to the
   replaced element a browser treats an `svg` as, it is what
   `image::inline_svg` registers a raster against, and any author `display`
   overrides it.

   `overflow: hidden` is SVG 2 §3.11 verbatim: "In the User Agent style sheet,
   overflow is overridden for the 'svg' element when it is not the root element
   of a stand-alone document ... to be hidden by default." It is the clip that
   keeps a `foreignObject`'s HTML inside the icon's box.

   §3.2.1's never-rendered element types have no direct representation in the
   rendering tree whatever their `display` value. The list is also declared, as
   data, in `image::inline_svg::NEVER_RENDERED_SVG_ELEMENTS`, and a test there
   asserts that constant against the specification text, so this rule and that
   list cannot drift apart. Hiding them here is also what keeps a `<title>`'s
   text and a `<style>`'s CSS out of the page's text layout. The rule matches
   by local name, which is the case the SVG namespace uses. */
svg { display: inline-block; overflow: hidden; }
clipPath, defs, desc, linearGradient, marker, mask, metadata, pattern,
radialGradient, script, style, title { display: none; }
"#;

/// The part of the user-agent sheet that depends on the document's scripting
/// mode.
///
/// §15.3.1's list of elements a user agent is expected to render with
/// `display: none` names `head`, `link`, `meta`, `script`, `style`, `template`,
/// `title` and the rest, and pointedly does **not** name `noscript`: whether
/// a `noscript` element's contents are the fallback a page serves to a user
/// without scripting, or inert text the parser never turned into elements, is
/// decided by the scripting mode (HTML 13.2.4.5, and the `noscript` rules at
/// 13.2.6.4.4 and 13.2.6.4.5).
///
/// So the rule is conditional, in the same way the parse is:
///
/// * **Scripting enabled.** The parser treats `noscript` contents as raw text,
///   so the element holds a single text node and nothing under it can load or
///   apply anything. The rule is still correct - a browser hides `noscript` -
///   and hiding it is what keeps that text out of the rendering.
/// * **Scripting disabled.** The contents are real markup, and the fallback is
///   the point: `<noscript><link rel=stylesheet href=fallback.css></noscript>`
///   must fetch and apply, and `<noscript><img src=a.png></noscript>` must
///   draw. Hiding the element would suppress exactly the content the page
///   intends this user to see.
const NOSCRIPT_HIDDEN_WHILE_SCRIPTING: &str = "noscript { display: none; }\n";

/// Whether a document's mode selects the quirks-mode rendering path.
///
/// This answers `Quirks` only, which is what the browser's own selector
/// context does (`render-browser`'s render worker tests
/// `document.quirks_mode() == QuirksMode::Quirks`). `LimitedQuirks` is
/// deliberately not included: the headless and browser paths must agree, and
/// changing which of the two modes counts is a decision about
/// limited-quirks-mode rules that the HTML standard specifies separately and
/// that neither path implements yet. See [`QUIRKS_STYLE_SHEET`] for what the
/// quirks path does apply.
const fn is_quirks_mode(mode: QuirksMode) -> bool {
    matches!(mode, QuirksMode::Quirks)
}

/// The part of the user-agent sheet that applies only to a quirks-mode
/// document.
///
/// **Citation.** Every rule below is quoted from the WHATWG HTML Living
/// Standard, "Rendering" chapter (<https://html.spec.whatwg.org/multipage/rendering.html>),
/// version of 25 September 2026, from the block each section gives under its
/// own "In quirks mode" heading. Each comment names the sub-item it is, so a
/// reviewer can check coverage against the document rather than against this
/// file's claims.
///
/// **What is *not* here, and why.** The standard's quirks-mode rules that are
/// not user-agent-sheet rules are the box-model changes in the original CSS 2
/// §9.2.1.1 list: unitless `line-height` is inherited as a number rather than
/// as a computed length, and percentage `height`/`margin`/`padding` are
/// treated as `auto` on table cells and on non-replaced inline boxes. Those are
/// solver behaviour in `render-layout`, not style-sheet rules, and they are
/// **not implemented** - see `docs/visual_fidelity_gaps.md`. They are also not
/// currently citable: CSS 2.1 has no quirks-mode section at all (§9.2.1.1
/// there is "Anonymous block boxes"), and the CSS 2 URLs now serve the CSS 2.1
/// text, so the list this comment describes could not be read from the
/// specification while writing it. An implementer must read the source
/// document first rather than take this paragraph as the rule.
///
/// The `list-style-position` rules of §15.3.7 are quoted in the specification
/// and are also **not implemented**, because `list-style-position` is not in
/// the property registry: a rule for a property nothing reads would be a
/// declaration of support the engine does not have.
///
/// §15.3.9 ("Margin collapsing quirks") is quoted in the specification as four
/// user-agent style sheet rules whose *conditions* are stated over the DOM -
/// "has no substantial previous siblings", "is blank" - so they are not
/// expressible as a selector. They are implemented in
/// [`quirks_margin_declarations`] instead, which is the same user-agent
/// origin and zero specificity the rules describe.
const QUIRKS_STYLE_SHEET: &str = r#"
/* §15.3.3 Flow content, "In quirks mode": a form's block-end margin. */
form { margin-bottom: 1em; }

/* §15.3.8 Tables, "In quirks mode": a table element's font, line height,
   white-space and text alignment all reset to their initial values, so they
   are inherited from no ancestor. All five have a consumer, and the three font
   properties are the ones `docs/visual_fidelity_gaps.md` S1 gave consumers. */
table {
  font-weight: initial;
  font-style: initial;
  font-size: initial;
  line-height: initial;
  white-space: initial;
  text-align: initial;
}

/* §15.3.10 Form controls, "In quirks mode": a text control's box is sized
   including its padding and border. */
input:not([type=image i]), textarea { box-sizing: border-box; }
"#;
fn ua_style_sheet(
    quirks_mode: QuirksMode,
    scripting_enabled: bool,
) -> std::borrow::Cow<'static, str> {
    let base = if scripting_enabled {
        std::borrow::Cow::Owned(format!("{UA_STYLE_SHEET}{NOSCRIPT_HIDDEN_WHILE_SCRIPTING}"))
    } else {
        std::borrow::Cow::Borrowed(UA_STYLE_SHEET)
    };
    if is_quirks_mode(quirks_mode) {
        std::borrow::Cow::Owned(format!("{base}{QUIRKS_STYLE_SHEET}"))
    } else {
        base
    }
}

/// HTML presentational attributes expressed as user-agent-origin CSS
/// declarations (HTML5 rendering §15.3). These let classic markup
/// (`bgcolor`, `width`, `align`, `cellpadding`, ...) style pages without a
/// stylesheet while remaining overridable by any author rule.
///
/// An `svg` element's `width`/`height` join them, for the same reason: SVG 2
/// §4.2 defines every presentation attribute by reference to its corresponding
/// CSS property, and an `svg` element's size comes from those two attributes
/// and its `viewBox` rather than from layout, which cannot measure foreign
/// content.
///
/// In a quirks-mode document this is also where §15.3.9's margin-collapsing
/// rules live, because their conditions are stated over the DOM rather than
/// over selectors.
fn presentational_hint_declarations(
    dom: &Dom,
    node: NodeId,
    quirks_mode: QuirksMode,
) -> Vec<Declaration> {
    fn declaration(name: &str, value: String) -> Declaration {
        Declaration {
            name: name.to_owned(),
            value,
            important: false,
        }
    }
    fn px_or_percent(raw: &str) -> String {
        let trimmed = raw.trim();
        if trimmed.ends_with('%') {
            trimmed.to_owned()
        } else {
            let digits = trimmed
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect::<String>();
            format!("{digits}px")
        }
    }
    let Some(NodeKind::Element(element)) = dom.node(node).map(Node::kind) else {
        return Vec::new();
    };
    let attribute = |name: &str| {
        dom.attribute(node, name)
            .ok()
            .flatten()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let tag = element.local_name.as_str();
    let mut hints = Vec::new();

    // bgcolor: background painting on the classic set of elements.
    if matches!(
        tag,
        "body" | "table" | "thead" | "tbody" | "tfoot" | "tr" | "td" | "th" | "col" | "colgroup"
    ) && let Some(bgcolor) = attribute("bgcolor")
    {
        hints.push(declaration("background-color", bgcolor.to_owned()));
    }

    // width/height presentational sizes.
    if matches!(
        tag,
        "table" | "td" | "th" | "img" | "video" | "col" | "colgroup" | "iframe" | "object"
    ) && let Some(width) = attribute("width")
    {
        hints.push(declaration("width", px_or_percent(width)));
    }
    if matches!(
        tag,
        "table" | "td" | "th" | "img" | "video" | "iframe" | "object"
    ) && let Some(height) = attribute("height")
    {
        hints.push(declaration("height", px_or_percent(height)));
    }

    // align: text alignment for cell/block content, geometry for
    // table/images.
    if let Some(align) = attribute("align") {
        let align = align.to_ascii_lowercase();
        match tag {
            "table" => match align.as_str() {
                "center" => {
                    hints.push(declaration("margin-left", "auto".to_owned()));
                    hints.push(declaration("margin-right", "auto".to_owned()));
                }
                "left" => hints.push(declaration("float", "left".to_owned())),
                "right" => hints.push(declaration("float", "right".to_owned())),
                _ => {}
            },
            "img" | "video" | "object" | "embed" => match align.as_str() {
                "left" => hints.push(declaration("float", "left".to_owned())),
                "right" => hints.push(declaration("float", "right".to_owned())),
                "center" => {
                    hints.push(declaration("margin-left", "auto".to_owned()));
                    hints.push(declaration("margin-right", "auto".to_owned()));
                    hints.push(declaration("display", "block".to_owned()));
                }
                _ => {}
            },
            "td" | "th" | "tr" | "thead" | "tbody" | "tfoot" | "caption" | "p" | "div" | "h1"
            | "h2" | "h3" | "h4" | "h5" | "h6" | "legend" | "col" | "colgroup" => {
                if matches!(align.as_str(), "left" | "right" | "center" | "justify") {
                    hints.push(declaration("text-align", align));
                }
            }
            _ => {}
        }
    }

    // valign: vertical alignment inside table parts.
    if matches!(
        tag,
        "tr" | "td" | "th" | "thead" | "tbody" | "tfoot" | "col" | "colgroup"
    ) && let Some(valign) = attribute("valign")
    {
        let valign = valign.to_ascii_lowercase();
        if matches!(valign.as_str(), "top" | "middle" | "bottom" | "baseline") {
            hints.push(declaration("vertical-align", valign));
        }
    }

    // cellpadding applies to the cells of a table; cells look up the
    // nearest ancestor table themselves.
    if matches!(tag, "td" | "th") {
        let mut ancestor = dom.parent(node);
        while let Some(current) = ancestor {
            if let Some(NodeKind::Element(parent)) = dom.node(current).map(Node::kind) {
                if parent.local_name.as_str() == "table" {
                    if let Some(cellpadding) = dom
                        .attribute(current, "cellpadding")
                        .ok()
                        .flatten()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        let digits = cellpadding
                            .chars()
                            .take_while(char::is_ascii_digit)
                            .collect::<String>();
                        hints.push(declaration("padding", format!("{digits}px")));
                    }
                    break;
                }
                if parent.local_name.as_str() == "table" {
                    break;
                }
            }
            ancestor = dom.parent(current);
        }
    }

    // cellspacing →border spacing; a positive border attr draws the grid.
    if tag == "table" {
        if let Some(cellspacing) = attribute("cellspacing") {
            hints.push(declaration("border-spacing", px_or_percent(cellspacing)));
        }
        if let Some(border) = attribute("border") {
            let digits = border
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
                .unwrap_or(0);
            if digits > 0 {
                hints.push(declaration("border-style", "outset".to_owned()));
                hints.push(declaration("border-width", format!("{digits}px")));
            }
        }
    }

    // hspace/vspace margins on replaced elements.
    if matches!(tag, "img" | "video" | "object" | "embed") {
        if let Some(hspace) = attribute("hspace") {
            let value = px_or_percent(hspace);
            hints.push(declaration("margin-left", value.clone()));
            hints.push(declaration("margin-right", value));
        }
        if let Some(vspace) = attribute("vspace") {
            let value = px_or_percent(vspace);
            hints.push(declaration("margin-top", value.clone()));
            hints.push(declaration("margin-bottom", value));
        }
    }

    // An `svg` element is sized by its own geometry attributes. The resolution
    // is shared with the rasteriser so the box and the pixels agree.
    if element.namespace == Namespace::Svg && element.local_name == "svg" {
        let geometry = svg_geometry(element);
        if let Some(width) = geometry.used_width() {
            hints.push(declaration("width", format!("{width}px")));
        }
        if let Some(height) = geometry.used_height() {
            hints.push(declaration("height", format!("{height}px")));
        }
    }

    // §15.3.8 Tables, "In quirks mode": a cell with a `nowrap` attribute that
    // also has a `width` attribute whose value parses as a *length* takes a
    // `white-space: normal` hint, overriding the `td[nowrap]` rule above. The
    // specification requires the width to be a length, so a percentage width -
    // which is the case that actually breaks a nowrap cell - does not qualify.
    //
    // `nowrap` is a boolean attribute, so its presence is read through the
    // unfiltered accessor: the `attribute` closure above drops an empty value,
    // and a boolean attribute's value *is* empty.
    if is_quirks_mode(quirks_mode)
        && matches!(tag, "td" | "th")
        && dom.attribute(node, "nowrap").ok().flatten().is_some()
        && attribute("width").is_some_and(|width| !width.trim().ends_with('%'))
    {
        hints.push(declaration("white-space", "normal".to_owned()));
    }

    if is_quirks_mode(quirks_mode) {
        hints.extend(quirks_margin_declarations(dom, node, element));
    }
    hints
}

/// HTML 15 §15.3.9 "Margin collapsing quirks", implemented rule by rule.
///
/// The specification states four user-agent style sheet rules whose conditions
/// are over the DOM, so they are expressed here as user-agent-origin
/// declarations of zero specificity - the same cascade position the rules
/// describe. `margin-block-start` and `margin-block-end` are written as the
/// physical `margin-top` and `margin-bottom` for the reason given at the top
/// of this file: the sheet is left-to-right because the layout solver consumes
/// physical longhands only.
///
/// The specification's two definitions, quoted:
///
/// > A node is substantial if it is a text node that is not inter-element
/// > whitespace, or if it is an element node.
///
/// > A node is blank if it is an element that contains no substantial nodes.
///
/// The conditions are deliberately not `:first-child`, `:last-child` or
/// `:empty`. A comment node before the paragraph makes the element the first
/// *child* while it still has no substantial previous siblings, and `:empty`
/// counts comments, so both would zero margins the specification keeps.
fn quirks_margin_declarations(dom: &Dom, node: NodeId, element: &ElementData) -> Vec<Declaration> {
    fn zero(name: &str) -> Declaration {
        Declaration {
            name: name.to_owned(),
            value: "0px".to_owned(),
            important: false,
        }
    }
    /// §15.3.9: "The elements with default margins are the following elements:
    /// blockquote, dir, dl, h1, h2, h3, h4, h5, h6, listing, menu, ol, p,
    /// plaintext, pre, ul, xmp." Note that `figure` is *not* among them, even
    /// though this crate's user-agent sheet gives it a block margin.
    const ELEMENTS_WITH_DEFAULT_MARGINS: [&str; 16] = [
        "blockquote",
        "dir",
        "dl",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "listing",
        "menu",
        "ol",
        "p",
        "plaintext",
        "pre",
        "ul",
    ];

    if !ELEMENTS_WITH_DEFAULT_MARGINS.contains(&element.local_name.as_str()) {
        return Vec::new();
    }
    let Some(parent_id) = dom.parent(node) else {
        return Vec::new();
    };
    let Some(NodeKind::Element(parent)) = dom.node(parent_id).map(Node::kind) else {
        return Vec::new();
    };
    let in_body = parent.local_name == "body";
    let in_cell = matches!(parent.local_name.as_str(), "td" | "th");
    if !in_body && !in_cell {
        return Vec::new();
    }

    let children = dom.children(parent_id).unwrap_or_default();
    let Some(position) = children.iter().position(|child| *child == node) else {
        return Vec::new();
    };
    let no_substantial_previous = children[..position]
        .iter()
        .all(|child| !is_substantial(dom, *child));
    let no_substantial_following = children[position + 1..]
        .iter()
        .all(|child| !is_substantial(dom, *child));
    let blank = is_blank(dom, node);

    let mut hints = Vec::new();
    // "In quirks mode, any element with default margins that is the child of a
    //  body, td, or th element and has no substantial previous siblings ...
    //  'margin-block-start' property to zero."
    if no_substantial_previous {
        hints.push(zero("margin-top"));
    }
    // "... and is blank, is expected to have a user-agent level style sheet
    //  rule that sets its 'margin-block-end' property to zero also."
    if no_substantial_previous && blank {
        hints.push(zero("margin-bottom"));
    }
    // "In quirks mode, any element with default margins that is the child of a
    //  td or th element, has no substantial following siblings, and is blank,
    //  is expected to have ... 'margin-block-start' ... to zero."
    if in_cell && no_substantial_following && blank {
        hints.push(zero("margin-top"));
    }
    // "In quirks mode, any p element that is the child of a td or th element
    //  and has no substantial following siblings, is expected to have ...
    //  'margin-block-end' ... to zero." Note this one is not conditioned on
    //  being blank, and names `p` rather than the whole default-margin set.
    if in_cell && element.local_name == "p" && no_substantial_following {
        hints.push(zero("margin-bottom"));
    }
    hints
}

/// §15.3.9: "A node is substantial if it is a text node that is not
/// inter-element whitespace, or if it is an element node."
///
/// A text node holding only ASCII whitespace is inter-element whitespace here:
/// that is the case the definition exists to exclude, and it is why
/// `<body>\n<p>` still has a paragraph with no substantial previous sibling.
fn is_substantial(dom: &Dom, node: NodeId) -> bool {
    match dom.node(node).map(Node::kind) {
        Some(NodeKind::Element(_)) => true,
        Some(NodeKind::Text(data)) => !data
            .chars()
            .all(|character| character.is_ascii_whitespace()),
        _ => false,
    }
}

/// §15.3.9: "A node is blank if it is an element that contains no substantial
/// nodes." The element itself is not one of the nodes it contains, so this
/// walks descendants only.
fn is_blank(dom: &Dom, node: NodeId) -> bool {
    let mut stack = dom.children(node).unwrap_or_default().to_vec();
    while let Some(current) = stack.pop() {
        let Some(current_ref) = dom.node(current) else {
            continue;
        };
        if is_substantial(dom, current) {
            return false;
        }
        stack.extend(current_ref.children().iter().copied());
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentLimits {
    /// Bound the DOM walk performed solely to discover author style sources.
    pub max_style_discovery_nodes: usize,
    pub max_embedded_style_sheets: usize,
    pub max_embedded_style_bytes: usize,
    /// Bounds all `<style>` and stylesheet `<link>` slots discovered in DOM
    /// order, including currently ineligible slots.
    pub max_author_style_slots: usize,
    pub max_external_style_sheets: usize,
    pub max_external_style_url_bytes: usize,
}

impl Default for DocumentLimits {
    fn default() -> Self {
        Self {
            max_style_discovery_nodes: 1_000_000,
            max_embedded_style_sheets: 4_096,
            max_embedded_style_bytes: 16 * 1_024 * 1_024,
            max_author_style_slots: 8_192,
            max_external_style_sheets: 4_096,
            max_external_style_url_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocumentRenderOptions {
    pub document_limits: DocumentLimits,
    pub computation_limits: ComputationLimits,
    pub formatting_limits: FormattingLimits,
    pub layout: LayoutOptions,
    /// Document-space origin painted into the layout viewport. Layout itself
    /// always uses `layout.viewport`; this offset only affects compositing.
    pub scroll_offset: PhysicalPoint,
    pub display_list: DisplayListBuilderOptions,
    pub raster_background: Color,
    /// The document's scripting mode, as HTML 13.2.4.5 defines it.
    ///
    /// It is a property of the parse, not a rendering preference, so it must
    /// agree with the mode the DOM was built with: use
    /// [`Document::parse_with_scripting`] to parse as a user agent with
    /// scripting turned off and set this to `false` to match. The only
    /// user-agent rule that depends on it is `noscript { display: none }`
    /// (see [`NOSCRIPT_HIDDEN_WHILE_SCRIPTING`]), and the two modes disagree
    /// about real content - with scripting disabled a `noscript` subtree is
    /// the fallback markup the page is serving.
    ///
    /// The default is `true`, which is rENDER's mode: it has a script
    /// execution engine, so it is a scripting-enabled user agent, and
    /// `Document::parse` parses accordingly. Every existing caller therefore
    /// keeps today's behaviour without naming this field.
    pub scripting_enabled: bool,
}

impl Default for DocumentRenderOptions {
    fn default() -> Self {
        let display_list = DisplayListBuilderOptions::default();
        Self {
            document_limits: DocumentLimits::default(),
            computation_limits: ComputationLimits::default(),
            formatting_limits: FormattingLimits::default(),
            layout: LayoutOptions::default(),
            scroll_offset: PhysicalPoint::default(),
            raster_background: display_list.palette.canvas,
            display_list,
            scripting_enabled: true,
        }
    }
}

#[derive(Clone, Copy)]
pub struct DocumentBackends<'a> {
    pub text_measurer: &'a dyn TextMeasurer,
    pub text_shaper: &'a dyn TextShaper,
    pub glyph_masks: &'a dyn GlyphMaskProvider,
}

impl std::fmt::Debug for DocumentBackends<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DocumentBackends { .. }")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentDiagnosticCode {
    ExternalStyleSheetUnsupported,
    ExternalStyleSheetUnresolved,
    InlineStyleUnsupported,
    MediaQueryUnsupported,
    NonCssStyleType,
    QuirksModeUnsupported,
    StyleDiscoveryNodeLimit,
    EmbeddedStyleLimit,
    EmbeddedStyleBytesLimit,
    AuthorStyleSlotLimit,
    ExternalStyleSheetLimit,
    ExternalStyleSheetUrlBytesLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentDiagnostic {
    pub node: Option<NodeId>,
    pub code: DocumentDiagnosticCode,
    pub message: String,
}

/// Why an author stylesheet slot is or is not applicable to the current
/// screen rendering environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorStyleEligibility {
    pub type_is_css: bool,
    pub media_matches: bool,
    /// False means some media query syntax could not yet be evaluated. Other
    /// valid entries in the comma-separated media list may still match.
    pub media_fully_supported: bool,
}

impl AuthorStyleEligibility {
    #[must_use]
    pub const fn is_eligible(self) -> bool {
        self.type_is_css && self.media_matches
    }
}

/// The source represented by an author style slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorStyleSource {
    Embedded,
    External {
        href: String,
        /// URL resolved against the caller-supplied document base URL.
        resolved_url: Option<Url>,
    },
}

/// One `<style>` or `<link rel=stylesheet href>` in DOM tree order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorStyleSlot {
    pub owner: NodeId,
    pub source_order: usize,
    pub source: AuthorStyleSource,
    pub eligibility: AuthorStyleEligibility,
}

/// Revision-bound stylesheet discovery result. Callers can fetch eligible
/// external slots in parallel, then inject their parsed sheets by key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorStyleDiscovery {
    pub revision: DomRevision,
    pub slots: Vec<AuthorStyleSlot>,
    pub diagnostics: Vec<DocumentDiagnostic>,
}

/// Stable identity for fetched CSS. The URL is the resolved link request URL,
/// not the post-redirect response URL; redirect bookkeeping remains a caller
/// responsibility.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExternalStyleSheetKey {
    pub owner: NodeId,
    pub requested_url: Url,
}

impl ExternalStyleSheetKey {
    #[must_use]
    pub const fn new(owner: NodeId, requested_url: Url) -> Self {
        Self {
            owner,
            requested_url,
        }
    }
}

/// Parsed external stylesheets supplied by a network/document coordinator.
#[derive(Clone, Debug, Default)]
pub struct ExternalStyleSheets {
    entries: HashMap<ExternalStyleSheetKey, StyleSheet>,
}

impl ExternalStyleSheets {
    pub fn insert(&mut self, key: ExternalStyleSheetKey, sheet: StyleSheet) -> Option<StyleSheet> {
        self.entries.insert(key, sheet)
    }

    pub fn insert_css(&mut self, key: ExternalStyleSheetKey, source: &str) -> Option<StyleSheet> {
        self.insert(key, parse_stylesheet(source))
    }

    #[must_use]
    pub fn get(&self, key: &ExternalStyleSheetKey) -> Option<&StyleSheet> {
        self.entries.get(key)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeStyleSheetDiagnostic {
    pub node: Option<NodeId>,
    pub diagnostic: StyleSheetDiagnostic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeComputationDiagnostic {
    pub node: NodeId,
    pub diagnostic: ComputationDiagnostic,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentRenderDiagnostics {
    pub document: Vec<DocumentDiagnostic>,
    pub style_sheets: Vec<NodeStyleSheetDiagnostic>,
    pub computed_styles: Vec<NodeComputationDiagnostic>,
    pub formatting: Vec<FormattingDiagnostic>,
    pub layout: Vec<LayoutDiagnostic>,
    pub display_list: Vec<DisplayListDiagnostic>,
    pub raster: Vec<RasterDiagnostic>,
}

#[derive(Clone, Debug)]
pub struct Document {
    dom: Dom,
    html_errors: Vec<HtmlParseError>,
    quirks_mode: QuirksMode,
}

impl Document {
    /// Parse an HTML document once. Subsequent script-driven updates should use
    /// [`Self::dom_mut`] and render the resulting DOM revision directly.
    ///
    /// rENDER is a scripting-enabled user agent, so this parses as one. Use
    /// [`Self::parse_with_scripting`] for the scripting-disabled half, and
    /// pair it with
    /// [`DocumentRenderOptions::scripting_enabled`] set to the same value so
    /// the user-agent sheet agrees with the DOM.
    #[must_use]
    pub fn parse(html: &str) -> Self {
        Self::parse_with_scripting(html, true)
    }

    /// Parse an HTML document in an explicitly chosen scripting mode.
    ///
    /// With scripting disabled, a `noscript` element's contents are markup
    /// rather than raw text (13.2.4.5, 13.2.6.4.5 and 13.2.6.4.7), which is
    /// what makes the no-JS fallback a user agent without scripting receives
    /// real elements. Render it with `DocumentRenderOptions` whose
    /// `scripting_enabled` is `false`; see
    /// [`NOSCRIPT_HIDDEN_WHILE_SCRIPTING`].
    #[must_use]
    pub fn parse_with_scripting(html: &str, scripting_enabled: bool) -> Self {
        let parsed = parse_document_with_scripting(html, scripting_enabled);
        Self {
            dom: parsed.dom,
            html_errors: parsed.errors,
            quirks_mode: parsed.quirks_mode,
        }
    }

    #[must_use]
    pub const fn dom(&self) -> &Dom {
        &self.dom
    }

    pub const fn dom_mut(&mut self) -> &mut Dom {
        &mut self.dom
    }

    #[must_use]
    pub fn html_errors(&self) -> &[HtmlParseError] {
        &self.html_errors
    }

    #[must_use]
    pub const fn quirks_mode(&self) -> QuirksMode {
        self.quirks_mode
    }

    /// Execute style, layout, display-list construction, and CPU painting for
    /// the current DOM revision.
    #[must_use]
    pub fn render(
        &self,
        options: DocumentRenderOptions,
        backends: DocumentBackends<'_>,
    ) -> DocumentRenderOutput {
        render_dom(
            &self.dom,
            self.quirks_mode,
            options,
            backends,
            None,
            &ExternalStyleSheets::default(),
            None,
        )
    }

    #[must_use]
    pub fn render_with_images(
        &self,
        options: DocumentRenderOptions,
        backends: DocumentBackends<'_>,
        images: &ImageResources,
    ) -> DocumentRenderOutput {
        render_dom(
            &self.dom,
            self.quirks_mode,
            options,
            backends,
            None,
            &ExternalStyleSheets::default(),
            Some(images),
        )
    }

    /// Discovers embedded and external author stylesheet slots for the current
    /// DOM revision. This is a pure discovery step and performs no I/O.
    ///
    /// Media evaluation runs with no viewport, so a viewport-dependent media
    /// feature cannot match here. Use
    /// [`Self::discover_author_style_slots_with_context`] when the viewport is
    /// known, or [`Self::render_with_external_style_sheets`], which supplies it.
    #[must_use]
    pub fn discover_author_style_slots(
        &self,
        base_url: &Url,
        limits: DocumentLimits,
    ) -> AuthorStyleDiscovery {
        self.discover_author_style_slots_with_context(base_url, limits, &MatchContext::default())
    }

    /// [`Self::discover_author_style_slots`] with the media-evaluation
    /// environment supplied by the caller, so `@media (min-width: ...)` on a
    /// `media` attribute is answered against the real viewport instead of
    /// being treated as undecidable.
    #[must_use]
    pub fn discover_author_style_slots_with_context(
        &self,
        base_url: &Url,
        limits: DocumentLimits,
        context: &MatchContext,
    ) -> AuthorStyleDiscovery {
        discover_author_style_slots(&self.dom, Some(base_url), limits, context)
    }

    /// Renders with parsed external stylesheets supplied by the caller. Slots
    /// are rediscovered for the current DOM revision and cascaded in DOM order.
    #[must_use]
    pub fn render_with_external_style_sheets(
        &self,
        options: DocumentRenderOptions,
        backends: DocumentBackends<'_>,
        base_url: &Url,
        external: &ExternalStyleSheets,
    ) -> DocumentRenderOutput {
        render_dom(
            &self.dom,
            self.quirks_mode,
            options,
            backends,
            Some(base_url),
            external,
            None,
        )
    }

    #[must_use]
    pub fn render_with_external_style_sheets_and_images(
        &self,
        options: DocumentRenderOptions,
        backends: DocumentBackends<'_>,
        base_url: &Url,
        external: &ExternalStyleSheets,
        images: &ImageResources,
    ) -> DocumentRenderOutput {
        render_dom(
            &self.dom,
            self.quirks_mode,
            options,
            backends,
            Some(base_url),
            external,
            Some(images),
        )
    }

    /// Deterministic reference path suitable for conformance tests.
    #[must_use]
    pub fn render_reference(&self, options: DocumentRenderOptions) -> DocumentRenderOutput {
        self.render(
            options,
            DocumentBackends {
                text_measurer: &SimpleTextMeasurer,
                text_shaper: &ReferenceTextShaper,
                glyph_masks: &NoGlyphMasks,
            },
        )
    }

    /// Deterministic reference render with caller-supplied external CSS.
    #[must_use]
    pub fn render_reference_with_external_style_sheets(
        &self,
        options: DocumentRenderOptions,
        base_url: &Url,
        external: &ExternalStyleSheets,
    ) -> DocumentRenderOutput {
        self.render_with_external_style_sheets(
            options,
            DocumentBackends {
                text_measurer: &SimpleTextMeasurer,
                text_shaper: &ReferenceTextShaper,
                glyph_masks: &NoGlyphMasks,
            },
            base_url,
            external,
        )
    }
}

#[derive(Clone, Debug)]
pub struct DocumentRenderOutput {
    pub revision: DomRevision,
    pub styles: BTreeMap<NodeId, ComputedStyle>,
    pub formatting: FormattingTree,
    pub layout: LayoutOutput,
    /// Effective, clamped document-space origin used for this raster.
    pub paint_viewport_origin: PhysicalPoint,
    pub display: DisplayListBuildOutput,
    pub raster: CpuRasterOutput,
    pub diagnostics: DocumentRenderDiagnostics,
}

fn render_dom(
    dom: &Dom,
    quirks_mode: QuirksMode,
    options: DocumentRenderOptions,
    backends: DocumentBackends<'_>,
    base_url: Option<&Url>,
    external: &ExternalStyleSheets,
    images: Option<&ImageResources>,
) -> DocumentRenderOutput {
    let stage_timing = std::env::var_os("RENDER_STAGE_TIMING").is_some();
    let mut stage_mark = std::time::Instant::now();
    let stage_elapsed = |name: &str, mark: &mut std::time::Instant| {
        if stage_timing {
            let now = std::time::Instant::now();
            eprintln!("stage {}: {:?}", name, now.duration_since(*mark));
            *mark = now;
        }
    };
    let ua_sheet = parse_stylesheet(&ua_style_sheet(quirks_mode, options.scripting_enabled));
    stage_elapsed("ua-parse", &mut stage_mark);
    // The same environment answers a stylesheet's `media` attribute, the
    // `@media` rules inside it, and selector matching, so the three cannot
    // disagree about which rules apply at this viewport or in this document
    // mode.
    //
    // `quirks_mode` is not decorative: the selector engine reads it for the
    // quirks-mode case-insensitive `id` and `class` matching of Selectors 4
    // §4.3 ("in quirks mode, class and ID selectors match ASCII
    // case-insensitively"), and the user-agent sheet below is built
    // differently for a quirks document.
    let match_context = MatchContext {
        viewport_width: Some(options.layout.viewport.width),
        viewport_height: Some(options.layout.viewport.height),
        quirks_mode: is_quirks_mode(quirks_mode),
        ..MatchContext::default()
    };
    let collected = collect_author_style_sheets(
        dom,
        base_url,
        external,
        options.document_limits,
        &match_context,
    );
    stage_elapsed("collect-sheets", &mut stage_mark);
    let mut cascade_inputs = Vec::with_capacity(collected.sheets.len().saturating_add(1));
    cascade_inputs.push(CascadeInput {
        sheet: &ua_sheet,
        origin: CascadeOrigin::UserAgent,
    });
    cascade_inputs.extend(collected.sheets.iter().map(|(_, sheet)| CascadeInput {
        sheet,
        origin: CascadeOrigin::Author,
    }));

    let styles = compute_document_styles_with_hints(
        dom,
        &cascade_inputs,
        &PropertyRegistry::standard_baseline(),
        &options.computation_limits,
        &match_context,
        &|node| presentational_hint_declarations(dom, node, quirks_mode),
    );
    stage_elapsed("cascade", &mut stage_mark);
    let formatting = build_formatting_tree(dom, &styles, &options.formatting_limits);
    stage_elapsed("formatting", &mut stage_mark);
    let layout = layout_formatting_tree_with_images(
        dom,
        &formatting,
        &styles,
        options.layout,
        backends.text_measurer,
        images,
    );
    stage_elapsed("layout", &mut stage_mark);
    let display = build_display_list_with_images(
        &layout.fragments,
        &formatting,
        &styles,
        options.display_list,
        backends.text_shaper,
        images,
    );
    stage_elapsed("display", &mut stage_mark);
    let paint_viewport_origin = layout.fragments.clamp_scroll_offset(options.scroll_offset);
    let raster = CpuRasterizer.rasterize_viewport_with_images(
        &display.list,
        options.raster_background,
        backends.glyph_masks,
        paint_viewport_origin,
        images,
    );
    stage_elapsed("raster", &mut stage_mark);

    let mut document_diagnostics = collected.diagnostics;
    if quirks_mode != QuirksMode::NoQuirks {
        document_diagnostics.insert(
            0,
            DocumentDiagnostic {
                node: None,
                code: DocumentDiagnosticCode::QuirksModeUnsupported,
                message: format!(
                    "{} CSS quirks are not implemented; standards-mode CSS semantics were used",
                    quirks_mode.as_str()
                ),
            },
        );
    }
    let diagnostics = DocumentRenderDiagnostics {
        document: document_diagnostics,
        style_sheets: std::iter::once((None, &ua_sheet))
            .chain(
                collected
                    .sheets
                    .iter()
                    .map(|(node, sheet)| (Some(*node), sheet)),
            )
            .flat_map(|(node, sheet)| {
                sheet
                    .diagnostics
                    .iter()
                    .cloned()
                    .map(move |diagnostic| NodeStyleSheetDiagnostic { node, diagnostic })
            })
            .collect(),
        computed_styles: styles
            .iter()
            .flat_map(|(node, style)| {
                style.diagnostics().iter().cloned().map(move |diagnostic| {
                    NodeComputationDiagnostic {
                        node: *node,
                        diagnostic,
                    }
                })
            })
            .collect(),
        formatting: formatting.diagnostics().to_vec(),
        layout: layout.diagnostics.clone(),
        display_list: display.diagnostics.clone(),
        raster: raster.diagnostics.clone(),
    };

    DocumentRenderOutput {
        revision: dom.revision(),
        styles,
        formatting,
        layout,
        paint_viewport_origin,
        display,
        raster,
        diagnostics,
    }
}

struct CollectedStyleSheets {
    sheets: Vec<(NodeId, StyleSheet)>,
    diagnostics: Vec<DocumentDiagnostic>,
}

fn collect_author_style_sheets(
    dom: &Dom,
    base_url: Option<&Url>,
    external: &ExternalStyleSheets,
    limits: DocumentLimits,
    context: &MatchContext,
) -> CollectedStyleSheets {
    let discovery = discover_author_style_slots(dom, base_url, limits, context);
    let mut sheets = Vec::new();
    let mut diagnostics = discovery.diagnostics;
    let mut style_bytes = 0_usize;
    let mut embedded_sheet_count = 0_usize;

    for slot in discovery.slots {
        if !slot.eligibility.is_eligible() {
            continue;
        }
        match slot.source {
            AuthorStyleSource::Embedded => collect_style_element(
                dom,
                slot.owner,
                limits,
                &mut embedded_sheet_count,
                &mut style_bytes,
                &mut sheets,
                &mut diagnostics,
            ),
            AuthorStyleSource::External {
                resolved_url: Some(requested_url),
                ..
            } => {
                let key = ExternalStyleSheetKey::new(slot.owner, requested_url.clone());
                if let Some(sheet) = external.get(&key) {
                    sheets.push((slot.owner, sheet.clone()));
                } else {
                    diagnostics.push(DocumentDiagnostic {
                        node: Some(slot.owner),
                        code: DocumentDiagnosticCode::ExternalStyleSheetUnsupported,
                        message: format!(
                            "external stylesheet bytes were not supplied for {requested_url}"
                        ),
                    });
                }
            }
            AuthorStyleSource::External {
                resolved_url: None, ..
            } if base_url.is_none() => diagnostics.push(DocumentDiagnostic {
                node: Some(slot.owner),
                code: DocumentDiagnosticCode::ExternalStyleSheetUnsupported,
                message: "external stylesheet requires a document base URL and supplied bytes"
                    .to_owned(),
            }),
            AuthorStyleSource::External { .. } => {}
        }
    }
    CollectedStyleSheets {
        sheets,
        diagnostics,
    }
}

fn discover_author_style_slots(
    dom: &Dom,
    base_url: Option<&Url>,
    limits: DocumentLimits,
    context: &MatchContext,
) -> AuthorStyleDiscovery {
    let mut discovery = StyleDiscoveryState::new(context);
    let mut stack = vec![dom.document()];
    let mut visited_nodes = 0_usize;

    while let Some(node_id) = stack.pop() {
        if visited_nodes >= limits.max_style_discovery_nodes {
            discovery.diagnostics.push(DocumentDiagnostic {
                node: None,
                code: DocumentDiagnosticCode::StyleDiscoveryNodeLimit,
                message: "style-source discovery stopped at its DOM node limit".to_owned(),
            });
            break;
        }
        visited_nodes += 1;
        let Some(node) = dom.node(node_id) else {
            continue;
        };
        if let NodeKind::Element(element) = node.kind() {
            discovery.inspect_element(node_id, element, base_url, limits);
        }
        if !matches!(node.kind(), NodeKind::Element(element) if element.local_name == "template") {
            stack.extend(node.children().iter().rev().copied());
        }
    }
    discovery.finish(dom.revision())
}

/// One discovery pass over the DOM, accumulating style slots and the
/// diagnostics the limits and the media evaluator produce.
struct StyleDiscoveryState<'a> {
    slots: Vec<AuthorStyleSlot>,
    diagnostics: Vec<DocumentDiagnostic>,
    /// The media-evaluation environment every slot's `media` attribute is
    /// answered in, so one pass cannot score two slots against different
    /// viewports.
    context: &'a MatchContext,
    source_order: usize,
    external_count: usize,
    external_url_bytes: usize,
    slot_limit_reported: bool,
    external_limit_reported: bool,
    external_bytes_limit_reported: bool,
}

impl<'a> StyleDiscoveryState<'a> {
    const fn new(context: &'a MatchContext) -> Self {
        Self {
            slots: Vec::new(),
            diagnostics: Vec::new(),
            context,
            source_order: 0,
            external_count: 0,
            external_url_bytes: 0,
            slot_limit_reported: false,
            external_limit_reported: false,
            external_bytes_limit_reported: false,
        }
    }

    fn inspect_element(
        &mut self,
        node: NodeId,
        element: &ElementData,
        base_url: Option<&Url>,
        limits: DocumentLimits,
    ) {
        if element.local_name == "style" {
            let eligibility = style_eligibility(node, element, &mut self.diagnostics, self.context);
            self.push_slot(
                AuthorStyleSlot {
                    owner: node,
                    source_order: self.source_order,
                    source: AuthorStyleSource::Embedded,
                    eligibility,
                },
                limits,
            );
            self.source_order = self.source_order.saturating_add(1);
        } else if element.local_name == "link"
            && is_stylesheet_link(element)
            && let Some(href) = attribute(element, "href")
        {
            self.discover_external(node, element, href, base_url, limits);
        }
    }

    fn discover_external(
        &mut self,
        node: NodeId,
        element: &ElementData,
        href: &str,
        base_url: Option<&Url>,
        limits: DocumentLimits,
    ) {
        let source_order = self.source_order;
        self.source_order = self.source_order.saturating_add(1);
        let eligibility = style_eligibility(node, element, &mut self.diagnostics, self.context);
        if self.external_count >= limits.max_external_style_sheets {
            self.report_external_count_limit(node);
            return;
        }
        let Some(next_url_bytes) = self.external_url_bytes.checked_add(href.len()) else {
            self.report_external_bytes_limit(node);
            return;
        };
        if next_url_bytes > limits.max_external_style_url_bytes {
            self.report_external_bytes_limit(node);
            return;
        }

        self.external_count += 1;
        self.external_url_bytes = next_url_bytes;
        let resolved_url = resolve_style_url(base_url, href);
        if base_url.is_some() && resolved_url.is_none() && eligibility.is_eligible() {
            self.diagnostics.push(DocumentDiagnostic {
                node: Some(node),
                code: DocumentDiagnosticCode::ExternalStyleSheetUnresolved,
                message: format!("could not resolve external stylesheet URL {href:?}"),
            });
        }
        self.push_slot(
            AuthorStyleSlot {
                owner: node,
                source_order,
                source: AuthorStyleSource::External {
                    href: href.to_owned(),
                    resolved_url,
                },
                eligibility,
            },
            limits,
        );
    }

    fn push_slot(&mut self, slot: AuthorStyleSlot, limits: DocumentLimits) {
        if self.slots.len() < limits.max_author_style_slots {
            self.slots.push(slot);
        } else if !self.slot_limit_reported {
            self.diagnostics.push(DocumentDiagnostic {
                node: Some(slot.owner),
                code: DocumentDiagnosticCode::AuthorStyleSlotLimit,
                message: "author stylesheet slot limit exceeded".to_owned(),
            });
            self.slot_limit_reported = true;
        }
    }

    fn report_external_count_limit(&mut self, node: NodeId) {
        if !self.external_limit_reported {
            self.diagnostics.push(DocumentDiagnostic {
                node: Some(node),
                code: DocumentDiagnosticCode::ExternalStyleSheetLimit,
                message: "external stylesheet discovery count limit exceeded".to_owned(),
            });
            self.external_limit_reported = true;
        }
    }

    fn report_external_bytes_limit(&mut self, node: NodeId) {
        if !self.external_bytes_limit_reported {
            self.diagnostics.push(DocumentDiagnostic {
                node: Some(node),
                code: DocumentDiagnosticCode::ExternalStyleSheetUrlBytesLimit,
                message: "external stylesheet URL byte limit exceeded".to_owned(),
            });
            self.external_bytes_limit_reported = true;
        }
    }

    fn finish(self, revision: DomRevision) -> AuthorStyleDiscovery {
        AuthorStyleDiscovery {
            revision,
            slots: self.slots,
            diagnostics: self.diagnostics,
        }
    }
}

fn is_stylesheet_link(element: &ElementData) -> bool {
    attribute(element, "rel").is_some_and(|rel| {
        rel.split_ascii_whitespace()
            .any(|token| token.eq_ignore_ascii_case("stylesheet"))
    })
}

fn style_eligibility(
    node: NodeId,
    element: &ElementData,
    diagnostics: &mut Vec<DocumentDiagnostic>,
    context: &MatchContext,
) -> AuthorStyleEligibility {
    let type_is_css = !attribute(element, "type").is_some_and(|kind| {
        !kind.trim().is_empty() && !kind.trim().eq_ignore_ascii_case("text/css")
    });
    if !type_is_css {
        diagnostics.push(DocumentDiagnostic {
            node: Some(node),
            code: DocumentDiagnosticCode::NonCssStyleType,
            message: "an author stylesheet slot with a non-CSS type was not applied".to_owned(),
        });
    }
    let media = evaluate_screen_media(attribute(element, "media"), context);
    if media.has_unsupported_query {
        diagnostics.push(DocumentDiagnostic {
            node: Some(node),
            code: DocumentDiagnosticCode::MediaQueryUnsupported,
            message: "the media list names a feature or syntax the media evaluator cannot answer"
                .to_owned(),
        });
    }
    AuthorStyleEligibility {
        type_is_css,
        media_matches: media.matches,
        media_fully_supported: !media.has_unsupported_query,
    }
}

fn resolve_style_url(base_url: Option<&Url>, href: &str) -> Option<Url> {
    let href = href.trim();
    match base_url {
        Some(base_url) => base_url.join(href).ok(),
        None => Url::parse(href).ok(),
    }
}

fn collect_style_element(
    dom: &Dom,
    node_id: NodeId,
    limits: DocumentLimits,
    embedded_sheet_count: &mut usize,
    style_bytes: &mut usize,
    sheets: &mut Vec<(NodeId, StyleSheet)>,
    diagnostics: &mut Vec<DocumentDiagnostic>,
) {
    let Some(node) = dom.node(node_id) else {
        return;
    };
    if *embedded_sheet_count >= limits.max_embedded_style_sheets {
        diagnostics.push(DocumentDiagnostic {
            node: Some(node_id),
            code: DocumentDiagnosticCode::EmbeddedStyleLimit,
            message: "embedded stylesheet count limit exceeded".to_owned(),
        });
        return;
    }

    let remaining_bytes = limits.max_embedded_style_bytes.saturating_sub(*style_bytes);
    let Some(css) = descendant_text_with_limit(dom, node, remaining_bytes) else {
        diagnostics.push(style_bytes_diagnostic(node_id));
        return;
    };
    let Some(next_style_bytes) = style_bytes.checked_add(css.len()) else {
        diagnostics.push(style_bytes_diagnostic(node_id));
        return;
    };
    *style_bytes = next_style_bytes;
    *embedded_sheet_count += 1;
    sheets.push((node_id, parse_stylesheet(&css)));
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MediaEvaluation {
    matches: bool,
    has_unsupported_query: bool,
}

/// Evaluate an author stylesheet's `media` attribute for a screen rendering.
///
/// Both answers come from the cascade's own evaluator, which is the only
/// evaluator that decides whether the sheet's rules apply. There is
/// deliberately no second parse here: a private copy that only understood
/// media *types* called every feature-bearing query unsupported - the next
/// word after `screen` is `and` - so `@media (min-width: 768px)` on a real
/// page produced a false `MediaQueryUnsupported` warning, and its sheet was
/// dropped rather than applied. One evaluator, one answer: a query reported
/// here as unsupported is one the cascade genuinely cannot evaluate, and a
/// query that matches here is one whose rules really were gathered.
fn evaluate_screen_media(media: Option<&str>, context: &MatchContext) -> MediaEvaluation {
    let Some(media) = media.map(str::trim).filter(|media| !media.is_empty()) else {
        return MediaEvaluation {
            matches: true,
            has_unsupported_query: false,
        };
    };
    MediaEvaluation {
        matches: media_query_list_matches(media, context),
        has_unsupported_query: !media_query_list_is_supported(media),
    }
}

fn descendant_text_with_limit(dom: &Dom, root: &Node, max_bytes: usize) -> Option<String> {
    let mut text = String::with_capacity(max_bytes.min(4_096));
    let mut stack = root.children().iter().rev().copied().collect::<Vec<_>>();
    while let Some(node) = stack.pop() {
        let Some(node) = dom.node(node) else {
            continue;
        };
        if let NodeKind::Text(data) = node.kind() {
            if text
                .len()
                .checked_add(data.len())
                .is_none_or(|length| length > max_bytes)
            {
                return None;
            }
            text.push_str(data);
        }
        stack.extend(node.children().iter().rev().copied());
    }
    Some(text)
}

fn attribute<'a>(element: &'a ElementData, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|attribute| attribute.namespace.is_none() && attribute.local_name == name)
        .map(|attribute| attribute.value.as_str())
}

fn style_bytes_diagnostic(node: NodeId) -> DocumentDiagnostic {
    DocumentDiagnostic {
        node: Some(node),
        code: DocumentDiagnosticCode::EmbeddedStyleBytesLimit,
        message: "embedded stylesheet byte limit exceeded".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AuthorStyleSlot, AuthorStyleSource, Document, DocumentDiagnosticCode, DocumentLimits,
        DocumentRenderOptions, ExternalStyleSheetKey, ExternalStyleSheets,
    };
    use crate::css::computed::ComputedValue;
    use crate::css::selector::{MatchContext, parse_selector_list, select_all};
    use crate::dom::{Dom, NodeId, NodeKind};
    use crate::image::{DecodedImage, ImageLimits, ImageResources, discover_images};
    use crate::layout::{FragmentKind, PhysicalPoint, PhysicalSize};
    use crate::paint::{Color, DisplayCommand};
    use url::Url;

    fn target_id(dom: &Dom, selector: &str) -> NodeId {
        let selectors = parse_selector_list(selector).expect("test selector must parse");
        select_all(dom, dom.document(), &selectors, &MatchContext::default())[0]
    }

    fn typed_css(
        document: &Document,
        render: &super::DocumentRenderOutput,
        selector: &str,
        property: &str,
    ) -> String {
        let node = target_id(document.dom(), selector);
        render.styles[&node]
            .typed(property)
            .unwrap_or_else(|| panic!("{property} must have a typed computed value"))
            .to_css()
    }

    /// The computed value as text, for properties with no typed representation.
    ///
    /// A typed value is the better assertion where one exists, because it is
    /// the value the consumers read. `white-space` has none, so the token-level
    /// computed value is the only thing there is to assert on.
    fn computed_css(
        document: &Document,
        render: &super::DocumentRenderOutput,
        selector: &str,
        property: &str,
    ) -> String {
        let node = target_id(document.dom(), selector);
        render.styles[&node]
            .get(property)
            .map_or_else(
                || panic!("{property} must have a computed value"),
                ComputedValue::css_text,
            )
            .to_owned()
    }

    fn external_key(slot: &AuthorStyleSlot) -> ExternalStyleSheetKey {
        let AuthorStyleSource::External {
            resolved_url: Some(url),
            ..
        } = &slot.source
        else {
            panic!("expected a resolved external stylesheet slot");
        };
        ExternalStyleSheetKey::new(slot.owner, url.clone())
    }

    #[test]
    fn embedded_author_css_flows_through_layout_and_paint() {
        let document = Document::parse(
            "<!doctype html><html><head><style>\
             #card { display:block; width:120px; height:40px; background-color:#2468ac }\
             </style></head><body><div id=card>hello</div></body></html>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());

        assert_eq!(render.revision, document.dom().revision());
        assert_eq!(render.display.list.dom_revision, render.revision);
        assert_eq!(render.raster.surface.width(), 1_280);
        assert!(render.display.list.items().iter().any(|item| {
            matches!(
                item.command,
                DisplayCommand::SolidRect { color, .. }
                    if color.red == 0x24 && color.green == 0x68 && color.blue == 0xac
            )
        }));
        assert!(render.diagnostics.document.is_empty());
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "test image coordinates are small, finite, non-negative CSS pixel positions"
    )]
    fn decoded_image_preserves_ratio_and_paints_pixels() {
        let document = Document::parse(
            "<!doctype html><style>body{margin:0}img{width:8px}</style><img src=hero.png>",
        );
        let url = Url::parse("https://example.test/").unwrap();
        let key = discover_images(document.dom(), &url, ImageLimits::default()).resources[0]
            .key
            .clone();
        let image =
            DecodedImage::from_pixels(2, 1, vec![Color::rgb(255, 0, 0), Color::rgb(0, 0, 255)])
                .unwrap();
        let mut images = ImageResources::default();
        images.insert(key, image, ImageLimits::default()).unwrap();
        let render = document.render_with_images(
            DocumentRenderOptions::default(),
            super::DocumentBackends {
                text_measurer: &crate::layout::SimpleTextMeasurer,
                text_shaper: &crate::paint::ReferenceTextShaper,
                glyph_masks: &crate::paint::NoGlyphMasks,
            },
            &images,
        );
        let node = target_id(document.dom(), "img");
        let size = render
            .layout
            .fragments
            .iter()
            .find_map(
                |fragment| match (&fragment.kind, fragment.source == Some(node)) {
                    (FragmentKind::Box(geometry), true) => Some(geometry.content_rect.size),
                    _ => None,
                },
            )
            .unwrap();
        assert_eq!(
            size,
            PhysicalSize {
                width: 8.0,
                height: 4.0
            }
        );
        let destination = render
            .display
            .list
            .items()
            .iter()
            .find_map(|item| match item.command {
                DisplayCommand::Image(image) => Some(image.destination),
                _ => None,
            })
            .unwrap();
        let sample_y = destination.origin.y as u32 + 1;
        assert_eq!(
            render
                .raster
                .surface
                .pixel(destination.origin.x as u32 + 1, sample_y),
            Some(Color::rgb(255, 0, 0))
        );
        assert_eq!(
            render
                .raster
                .surface
                .pixel(destination.origin.x as u32 + 6, sample_y),
            Some(Color::rgb(0, 0, 255))
        );
    }

    #[test]
    fn unloaded_image_uses_html_or_default_dimensions() {
        let document = Document::parse(
            "<!doctype html><style>body{margin:0}</style><img id=a src=a width=40 height=20><img id=b src=b>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());
        let size = |selector| {
            let node = target_id(document.dom(), selector);
            render
                .layout
                .fragments
                .iter()
                .find_map(
                    |fragment| match (&fragment.kind, fragment.source == Some(node)) {
                        (FragmentKind::Box(geometry), true) => Some(geometry.content_rect.size),
                        _ => None,
                    },
                )
                .unwrap()
        };
        assert_eq!(
            size("#a"),
            PhysicalSize {
                width: 40.0,
                height: 20.0
            }
        );
        assert_eq!(
            size("#b"),
            PhysicalSize {
                width: 300.0,
                height: 150.0
            }
        );
    }

    #[test]
    fn unsupported_css_sources_are_never_silently_ignored() {
        let document = Document::parse(
            "<!doctype html><link rel=stylesheet href=theme.css>\
             <link rel=stylesheet>\
             <style media='screen and (hover: hover)'>body { color:red }</style>\
             <body style='color:blue'></body>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());
        let codes = render
            .diagnostics
            .document
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();

        assert!(codes.contains(&DocumentDiagnosticCode::ExternalStyleSheetUnsupported));
        assert!(codes.contains(&DocumentDiagnosticCode::MediaQueryUnsupported));
        assert_eq!(
            typed_css(&document, &render, "body", "color"),
            "rgb(0, 0, 255)"
        );
        assert_eq!(
            codes
                .iter()
                .filter(|code| **code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported)
                .count(),
            1
        );
    }

    /// A media feature the cascade really can evaluate is neither reported as
    /// unsupported nor used to drop the sheet.
    ///
    /// It used to be both. A private media evaluator in this file only
    /// understood media *types*, and it marked any query carrying a feature
    /// unsupported because the next word after `screen` is `and` - so every
    /// `@media (min-width: 768px)` on a real page produced a false
    /// `MediaQueryUnsupported` warning while the same query was evaluated
    /// correctly by the cascade. That is the shape of defect this project has
    /// already paid for twice with stale unsupported-feature claims.
    #[test]
    fn a_feature_bearing_media_query_is_evaluated_and_not_reported_unsupported() {
        let document = Document::parse(
            "<!doctype html><style media='screen and (min-width: 768px)'>#query { color:red }</style>\
             <style media='(min-width: 100000px)'>#too-wide { color:red }</style>\
             <p id=query></p><p id=too-wide></p>",
        );
        let options = DocumentRenderOptions {
            layout: crate::layout::LayoutOptions {
                viewport: PhysicalSize {
                    width: 1_280.0,
                    height: 720.0,
                },
                ..crate::layout::LayoutOptions::default()
            },
            ..DocumentRenderOptions::default()
        };
        let render = document.render_reference(options);

        // The matching sheet is applied...
        assert_eq!(
            typed_css(&document, &render, "#query", "color"),
            "rgb(255, 0, 0)"
        );
        // ...and the non-matching one is not, from a real comparison rather
        // than from a parse that could not read it.
        assert_eq!(
            typed_css(&document, &render, "#too-wide", "color"),
            "canvastext"
        );
        assert!(
            !render.diagnostics.document.iter().any(|diagnostic| {
                diagnostic.code == DocumentDiagnosticCode::MediaQueryUnsupported
            }),
            "a media feature the cascade evaluates must not be reported unsupported"
        );
    }

    /// The default viewport is narrow enough that a `min-width` query the
    /// cascade understands still gates its sheet, which is the other half of
    /// "evaluated": support and matching are different questions.
    #[test]
    fn a_feature_bearing_media_query_gates_its_sheet_against_the_viewport() {
        let document = Document::parse(
            "<!doctype html><style media='(min-width: 100000px)'>#target { color:red }</style>\
             <p id=target></p>",
        );
        let discovery = Url::parse("https://example.test/index.html").expect("base URL");
        let slots = document
            .discover_author_style_slots_with_context(
                &discovery,
                DocumentLimits::default(),
                &MatchContext {
                    viewport_width: Some(1_280.0),
                    viewport_height: Some(720.0),
                    ..MatchContext::default()
                },
            )
            .slots;

        assert_eq!(slots.len(), 1);
        assert!(slots[0].eligibility.media_fully_supported);
        assert!(!slots[0].eligibility.media_matches);
        assert!(!slots[0].eligibility.is_eligible());
    }

    #[test]
    fn ua_display_defaults_cover_html_structures_without_hiding_inline_content() {
        let document = Document::parse(
            "<!doctype html><html><head><title>x</title></head><body>\
             <main id=main><span id=inline></span></main>\
             <details><summary id=summary>summary</summary></details>\
             <table><colgroup><col id=column></colgroup></table>\
             <input id=control><input id=hidden-control type=hidden>\
             <div id=hidden hidden>hidden</div></body></html>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());

        assert_eq!(typed_css(&document, &render, "#main", "display"), "block");
        assert_eq!(
            typed_css(&document, &render, "#inline", "display"),
            "inline"
        );
        assert_eq!(
            typed_css(&document, &render, "#summary", "display"),
            "block flow list-item"
        );
        assert_eq!(
            typed_css(&document, &render, "#column", "display"),
            "table-column"
        );
        // A text input centres its value in an inline flex box (HTML §15.5).
        assert_eq!(
            typed_css(&document, &render, "#control", "display"),
            "inline flex"
        );
        assert_eq!(
            typed_css(&document, &render, "#hidden-control", "display"),
            "none"
        );
        assert_eq!(typed_css(&document, &render, "#hidden", "display"), "none");
    }

    #[test]
    fn inline_style_hides_template_text_and_wins_author_specificity() {
        let document = Document::parse(
            "<!doctype html><style>#template { display:block !important; color:red !important }</style>\
             <textarea id=template style='display:none !important; color:blue !important'><div>raw template</div></textarea>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());

        assert_eq!(
            typed_css(&document, &render, "#template", "display"),
            "none"
        );
        assert_eq!(
            typed_css(&document, &render, "#template", "color"),
            "rgb(0, 0, 255)"
        );
        let template = target_id(document.dom(), "#template");
        assert!(
            render
                .layout
                .fragments
                .iter()
                .all(|fragment| fragment.source != Some(template))
        );
    }

    #[test]
    fn simple_screen_media_is_evaluated_and_unsupported_queries_are_diagnosed() {
        let document = Document::parse(
            "<!doctype html><style media='only screen'>#screen { color: red }</style>\
             <style media=print>#print { color: red }</style>\
             <style media='not print'>#not-print { color: red }</style>\
             <style media=speech>#speech { color: red }</style>\
             <style media='screen and (hover: hover)'>#query { color: red }</style>\
             <style type=text/plain>#wrong-type { color: red }</style>\
             <p id=screen></p><p id=print></p><p id=not-print></p><p id=speech></p>\
             <p id=query></p><p id=wrong-type></p>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());

        assert_eq!(
            typed_css(&document, &render, "#screen", "color"),
            "rgb(255, 0, 0)"
        );
        assert_eq!(
            typed_css(&document, &render, "#print", "color"),
            "canvastext"
        );
        assert_eq!(
            typed_css(&document, &render, "#not-print", "color"),
            "rgb(255, 0, 0)"
        );
        assert_eq!(
            typed_css(&document, &render, "#speech", "color"),
            "canvastext"
        );
        assert_eq!(
            typed_css(&document, &render, "#query", "color"),
            "canvastext"
        );
        assert_eq!(
            typed_css(&document, &render, "#wrong-type", "color"),
            "canvastext"
        );
        assert!(render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::MediaQueryUnsupported
        }));
        assert!(
            render
                .diagnostics
                .document
                .iter()
                .any(|diagnostic| { diagnostic.code == DocumentDiagnosticCode::NonCssStyleType })
        );
    }

    #[test]
    fn style_text_is_collected_again_after_dynamic_dom_mutation() {
        let mut document = Document::parse(
            "<!doctype html><style id=theme>#card { background-color: red }</style>\
             <div id=card>card</div>",
        );
        let first = document.render_reference(DocumentRenderOptions::default());
        assert_eq!(
            typed_css(&document, &first, "#card", "background-color"),
            "rgb(255, 0, 0)"
        );

        let style = target_id(document.dom(), "#theme");
        let text = document.dom().children(style).expect("style children")[0];
        document
            .dom_mut()
            .set_character_data(text, "#card { background-color: blue }")
            .expect("style text mutation succeeds");
        let second = document.render_reference(DocumentRenderOptions::default());

        assert!(second.revision > first.revision);
        assert_eq!(
            typed_css(&document, &second, "#card", "background-color"),
            "rgb(0, 0, 255)"
        );
    }

    #[test]
    fn external_and_embedded_sheets_cascade_in_dom_slot_order() {
        let document = Document::parse(
            "<!doctype html><html><head>\
             <style>#target { color: #ff0000 }</style>\
             <link rel=stylesheet href=css/a.css>\
             <style>#target { color: #00ff00 }</style>\
             <link rel=stylesheet href=css/b.css>\
             </head><body><p id=target>target</p></body></html>",
        );
        let base = Url::parse("https://example.test/pages/index.html").expect("base URL");
        let discovery = document.discover_author_style_slots(&base, DocumentLimits::default());

        assert_eq!(discovery.revision, document.dom().revision());
        assert_eq!(discovery.slots.len(), 4);
        assert_eq!(
            discovery
                .slots
                .iter()
                .map(|slot| slot.source_order)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        let AuthorStyleSource::External {
            resolved_url: Some(first_url),
            ..
        } = &discovery.slots[1].source
        else {
            panic!("second slot must be external");
        };
        assert_eq!(first_url.as_str(), "https://example.test/pages/css/a.css");

        let mut external = ExternalStyleSheets::default();
        external.insert_css(
            external_key(&discovery.slots[1]),
            "#target { color: #0000ff }",
        );
        let without_last = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &without_last, "#target", "color"),
            "rgb(0, 255, 0)"
        );
        assert!(without_last.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.node == Some(discovery.slots[3].owner)
                && diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported
        }));

        external.insert_css(
            external_key(&discovery.slots[3]),
            "#target { color: #000000 }",
        );
        let complete = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &complete, "#target", "color"),
            "rgb(0, 0, 0)"
        );
        assert!(!complete.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported
                || diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnresolved
        }));
    }

    #[test]
    fn external_discovery_models_media_type_and_resolution_failures() {
        let document = Document::parse(
            "<!doctype html><head>\
             <link rel=stylesheet href=print.css media=print>\
             <link rel=stylesheet href=plain.css type=text/plain>\
             <link rel=stylesheet href=query.css media='screen and (hover: hover)'>\
             <link rel=stylesheet href=screen.css media='only screen'>\
             <link rel=stylesheet href='http://['>\
             <link rel=stylesheet href=wide.css media='(min-width: 100000px)'>\
             </head><body><p id=target></p></body>",
        );
        let base = Url::parse("https://example.test/base/page.html").expect("base URL");
        let discovery = document.discover_author_style_slots(&base, DocumentLimits::default());

        assert_eq!(discovery.slots.len(), 6);
        assert!(!discovery.slots[0].eligibility.media_matches);
        assert!(!discovery.slots[1].eligibility.type_is_css);
        assert!(!discovery.slots[2].eligibility.media_fully_supported);
        assert!(discovery.slots[3].eligibility.is_eligible());
        assert!(matches!(
            discovery.slots[4].source,
            AuthorStyleSource::External {
                resolved_url: None,
                ..
            }
        ));
        // A feature the engine evaluates is supported syntax even when the
        // environment cannot answer it: this discovery pass runs with no
        // viewport, so the query cannot match, but nothing is missing.
        assert!(discovery.slots[5].eligibility.media_fully_supported);
        assert!(!discovery.slots[5].eligibility.media_matches);
        assert!(discovery.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnresolved
        }));
        assert!(!discovery.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::MediaQueryUnsupported
                && diagnostic.node == Some(discovery.slots[5].owner)
        }));

        let mut external = ExternalStyleSheets::default();
        external.insert_css(
            external_key(&discovery.slots[3]),
            "#target { color: #ff0000 }",
        );
        let render = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &render, "#target", "color"),
            "rgb(255, 0, 0)"
        );
        assert!(!render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported
        }));
        assert!(render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnresolved
        }));
    }

    #[test]
    fn external_style_discovery_limits_fail_closed() {
        let document = Document::parse(
            "<!doctype html><link rel=stylesheet href=a.css>\
             <link rel=stylesheet href=b.css><style>p { color:red }</style>",
        );
        let base = Url::parse("https://example.test/index.html").expect("base URL");
        let discovery = document.discover_author_style_slots(
            &base,
            DocumentLimits {
                max_external_style_sheets: 1,
                max_author_style_slots: 1,
                ..DocumentLimits::default()
            },
        );

        assert_eq!(discovery.slots.len(), 1);
        assert!(discovery.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetLimit
        }));
        assert!(
            discovery.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == DocumentDiagnosticCode::AuthorStyleSlotLimit
            })
        );
    }

    #[test]
    fn dynamically_inserted_and_retargeted_link_is_rediscovered() {
        let mut document = Document::parse(
            "<!doctype html><html><head id=head></head>\
             <body><p id=target>target</p></body></html>",
        );
        let base = Url::parse("https://example.test/index.html").expect("base URL");
        let initial_revision = document.dom().revision();
        assert!(
            document
                .discover_author_style_slots(&base, DocumentLimits::default())
                .slots
                .is_empty()
        );

        let head = target_id(document.dom(), "#head");
        let link = document.dom_mut().create_element("link");
        document
            .dom_mut()
            .set_attribute(link, "rel", "stylesheet")
            .expect("set rel");
        document
            .dom_mut()
            .set_attribute(link, "href", "old.css")
            .expect("set href");
        document
            .dom_mut()
            .append_child(head, link)
            .expect("insert stylesheet link");
        let old_discovery = document.discover_author_style_slots(&base, DocumentLimits::default());
        assert!(old_discovery.revision > initial_revision);
        assert_eq!(old_discovery.slots.len(), 1);

        let mut external = ExternalStyleSheets::default();
        external.insert_css(
            external_key(&old_discovery.slots[0]),
            "#target { color: #ff0000 }",
        );
        let old_render = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &old_render, "#target", "color"),
            "rgb(255, 0, 0)"
        );

        document
            .dom_mut()
            .set_attribute(link, "href", "new.css")
            .expect("retarget href");
        let new_discovery = document.discover_author_style_slots(&base, DocumentLimits::default());
        assert!(new_discovery.revision > old_discovery.revision);
        let stale_render = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &stale_render, "#target", "color"),
            "canvastext"
        );
        assert!(stale_render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.node == Some(link)
                && diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported
        }));

        external.insert_css(
            external_key(&new_discovery.slots[0]),
            "#target { color: #0000ff }",
        );
        let new_render = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &new_render, "#target", "color"),
            "rgb(0, 0, 255)"
        );
        assert!(!new_render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported
        }));
    }

    /// §15.3.1's list of elements a user agent renders with `display: none`
    /// pointedly does not name `noscript`, because whether its contents are
    /// the fallback a page serves to a user without scripting or inert text
    /// the parser never turned into elements is decided by the scripting mode
    /// (HTML 13.2.4.5, and the `noscript` rules at 13.2.6.4.4 and 13.2.6.4.5).
    /// One DOM, two user agents, two answers.
    #[test]
    fn the_noscript_display_rule_follows_the_scripting_mode() {
        let document = Document::parse_with_scripting(
            "<!doctype html><body><noscript><img id=fallback src=fallback.png></noscript>",
            false,
        );
        let hidden = document.render_reference(DocumentRenderOptions {
            scripting_enabled: true,
            ..DocumentRenderOptions::default()
        });
        let shown = document.render_reference(DocumentRenderOptions {
            scripting_enabled: false,
            ..DocumentRenderOptions::default()
        });

        assert_eq!(typed_css(&document, &hidden, "noscript", "display"), "none");
        assert_eq!(
            typed_css(&document, &shown, "noscript", "display"),
            "inline"
        );
    }

    /// With scripting disabled a `noscript` element's contents are real markup,
    /// and that is the fallback the page is serving such a user. Hiding the
    /// element suppressed exactly the content it exists to provide.
    #[test]
    fn scripting_disabled_noscript_fallback_markup_is_laid_out() {
        let document = Document::parse_with_scripting(
            "<!doctype html><body><noscript><img id=fallback src=fallback.png \
             width=40 height=20></noscript>",
            false,
        );
        let render = document.render_reference(DocumentRenderOptions {
            scripting_enabled: false,
            ..DocumentRenderOptions::default()
        });
        let fallback = target_id(document.dom(), "#fallback");
        let size = render.layout.fragments.iter().find_map(|fragment| {
            match (&fragment.kind, fragment.source == Some(fallback)) {
                (FragmentKind::Box(geometry), true) => Some(geometry.content_rect.size),
                _ => None,
            }
        });

        assert_eq!(
            size,
            Some(PhysicalSize {
                width: 40.0,
                height: 20.0
            }),
            "a no-JS fallback image is content the page serves on purpose"
        );
    }

    /// With scripting enabled the same markup produces a `noscript` element
    /// holding one text node and no elements at all, so the `display: none`
    /// rule hides inert text. This is the fact the rule's default rests on,
    /// and it is asserted here rather than assumed: it is what made the
    /// unconditional form harmless and what makes it wrong for the other mode.
    #[test]
    fn scripting_enabled_noscript_contents_are_inert_text() {
        let document = Document::parse(
            "<!doctype html><body><noscript><img id=fallback src=fallback.png></noscript>",
        );
        let noscript = target_id(document.dom(), "noscript");
        let children = document
            .dom()
            .children(noscript)
            .expect("noscript children");

        assert!(
            children.iter().all(|child| {
                matches!(
                    document.dom().node(*child).map(crate::dom::Node::kind),
                    Some(NodeKind::Text(_))
                )
            }),
            "with scripting enabled a noscript element's contents are one text node"
        );
        assert!(
            parse_selector_list("#fallback")
                .map(|selectors| {
                    select_all(
                        document.dom(),
                        document.dom().document(),
                        &selectors,
                        &MatchContext::default(),
                    )
                    .is_empty()
                })
                .expect("valid selector")
        );
        assert_eq!(
            typed_css(
                &document,
                &document.render_reference(DocumentRenderOptions::default()),
                "noscript",
                "display"
            ),
            "none"
        );
    }

    #[test]
    fn styles_in_template_contents_are_inert() {
        let mut document = Document::parse(
            "<!doctype html><template id=holder></template><p id=target>target</p>",
        );
        let template = target_id(document.dom(), "#holder");
        let style = document.dom_mut().create_element("style");
        let css = document.dom_mut().create_text("#target { color: red }");
        document
            .dom_mut()
            .append_child(style, css)
            .expect("style accepts text");
        document
            .dom_mut()
            .append_child(template, style)
            .expect("test DOM represents inert template contents");

        let render = document.render_reference(DocumentRenderOptions::default());
        assert_eq!(
            typed_css(&document, &render, "#target", "color"),
            "canvastext"
        );
    }

    #[test]
    fn style_resource_limits_fail_closed_without_preallocating_oversized_css() {
        let document = Document::parse(
            "<!doctype html><style>xxxxxxxxxxxxxxxxxxxx</style>\
             <style>p{color:red}</style><p id=target>target</p>",
        );
        let options = DocumentRenderOptions {
            document_limits: DocumentLimits {
                max_embedded_style_bytes: 12,
                ..DocumentLimits::default()
            },
            ..DocumentRenderOptions::default()
        };
        let render = document.render_reference(options);

        assert_eq!(
            typed_css(&document, &render, "#target", "color"),
            "rgb(255, 0, 0)"
        );
        assert_eq!(
            render
                .diagnostics
                .document
                .iter()
                .filter(|diagnostic| {
                    diagnostic.code == DocumentDiagnosticCode::EmbeddedStyleBytesLimit
                })
                .count(),
            1
        );
    }

    #[test]
    fn non_applicable_styles_do_not_consume_sheet_limit() {
        let document = Document::parse(
            "<!doctype html><style type=text/plain>p { color: blue }</style>\
             <style>p { color: red }</style><style>p { color: green }</style><p id=target></p>",
        );
        let options = DocumentRenderOptions {
            document_limits: DocumentLimits {
                max_embedded_style_sheets: 1,
                ..DocumentLimits::default()
            },
            ..DocumentRenderOptions::default()
        };
        let render = document.render_reference(options);

        assert_eq!(
            typed_css(&document, &render, "#target", "color"),
            "rgb(255, 0, 0)"
        );
        assert!(
            render
                .diagnostics
                .document
                .iter()
                .any(|diagnostic| { diagnostic.code == DocumentDiagnosticCode::NonCssStyleType })
        );
        assert!(
            render.diagnostics.document.iter().any(|diagnostic| {
                diagnostic.code == DocumentDiagnosticCode::EmbeddedStyleLimit
            })
        );
    }

    /// The parse already computes the three-way mode, and the selector engine
    /// already reads `MatchContext::quirks_mode` (Selectors 4 §4.3: "in quirks
    /// mode, class and ID selectors match ASCII case-insensitively"). What was
    /// missing was the wiring: the headless render path built its
    /// `MatchContext` from `MatchContext::default()`, so a quirks document was
    /// matched with standards-mode semantics while `render-browser` - which
    /// does set the flag - matched it the other way. The two paths disagreed
    /// about the same DOM.
    #[test]
    fn quirks_mode_reaches_selector_matching_on_the_headless_path() {
        // No doctype, so the parser puts the document in quirks mode.
        let quirks = Document::parse(
            "<html><body><style>#Foo, .Bar { color: #ff0000 }</style>\
             <p id=foo class=bar>target</p></body></html>",
        );
        assert_eq!(quirks.quirks_mode().as_str(), "quirks");
        let standards = Document::parse(
            "<!doctype html><html><body><style>#Foo, .Bar { color: #ff0000 }</style>\
             <p id=foo class=bar>target</p></body></html>",
        );
        assert_eq!(standards.quirks_mode().as_str(), "no-quirks");

        assert_eq!(
            typed_css(
                &quirks,
                &quirks.render_reference(DocumentRenderOptions::default()),
                "p",
                "color"
            ),
            "rgb(255, 0, 0)",
            "quirks mode matches id and class selectors ASCII case-insensitively"
        );
        assert_eq!(
            typed_css(
                &standards,
                &standards.render_reference(DocumentRenderOptions::default()),
                "p",
                "color"
            ),
            "canvastext",
            "standards mode matches them case-sensitively"
        );
    }

    /// §15.3.8 Tables, "In quirks mode": a table element's inherited typography
    /// and text alignment reset to their initial values.
    #[test]
    fn quirks_mode_resets_a_table_elements_inherited_properties() {
        let markup = "<body><div style='text-align:right; white-space:pre'>\
             <table id=target><tr><td>cell</td></tr></table></div>";
        let quirks = Document::parse(markup);
        let standards = Document::parse(&format!("<!doctype html>{markup}"));

        assert_eq!(
            typed_css(
                &quirks,
                &quirks.render_reference(DocumentRenderOptions::default()),
                "#target",
                "text-align"
            ),
            "start"
        );
        assert_eq!(
            computed_css(
                &quirks,
                &quirks.render_reference(DocumentRenderOptions::default()),
                "#target",
                "white-space"
            ),
            "normal"
        );
        assert_eq!(
            typed_css(
                &standards,
                &standards.render_reference(DocumentRenderOptions::default()),
                "#target",
                "text-align"
            ),
            "right",
            "standards mode inherits text-align from the ancestor"
        );
    }

    /// §15.3.9's four margin-collapsing rules, each asserted against the
    /// standards-mode result for the same markup so the pair cannot both be
    /// wrong.
    #[test]
    fn quirks_mode_collapses_the_default_margins_of_edge_elements() {
        fn margins(document: &Document, selector: &str) -> (String, String) {
            let render = document.render_reference(DocumentRenderOptions::default());
            (
                typed_css(document, &render, selector, "margin-top"),
                typed_css(document, &render, selector, "margin-bottom"),
            )
        }
        let pair = |markup: &str| {
            (
                Document::parse(markup),
                Document::parse(&format!("<!doctype html>{markup}")),
            )
        };

        // Rule 1: first child of a body, no substantial previous siblings.
        let (document, standards) = pair("<body><p id=first>one</p><p id=second>two</p>");
        assert_ne!(
            margins(&document, "#first").0,
            margins(&standards, "#first").0,
            "a leading paragraph's block-start margin is zeroed in quirks mode"
        );
        assert_eq!(
            margins(&document, "#first").0,
            "0px",
            "§15.3.9 rule 1 zeroes the block-start margin"
        );
        assert_eq!(
            margins(&document, "#second").0,
            margins(&standards, "#second").0,
            "a later paragraph keeps its default margin"
        );

        // A comment before the paragraph makes it the first *child* but not a
        // paragraph with no substantial previous siblings, so `:first-child`
        // would be the wrong test and the rule must still fire.
        let (document, standards) = pair("<body><!--c--><p id=only>one</p></body>");
        assert_eq!(margins(&document, "#only").0, "0px");
        assert_ne!(
            margins(&document, "#only").0,
            margins(&standards, "#only").0
        );
        assert_eq!(
            margins(&document, "#only").1,
            margins(&standards, "#only").1
        );

        // Rule 2: the same, and blank, so the block-end margin goes too.
        let (document, standards) = pair("<body><p id=blank></p><p id=full>text</p></body>");
        assert_eq!(
            margins(&document, "#blank"),
            ("0px".to_owned(), "0px".to_owned())
        );
        assert_eq!(
            margins(&document, "#full").0,
            margins(&standards, "#full").0,
            "a paragraph with content keeps its default block-start margin"
        );

        // A leading element that is *not* in the default-margin set does not
        // make the following paragraph a first child in the rule's sense, and
        // it is substantial, so the margin stays.
        let (document, standards) = pair("<body><div>lead</div><p id=after>one</p></body>");
        assert_eq!(
            margins(&document, "#after").0,
            margins(&standards, "#after").0
        );

        // Rules 3 and 4. Both need the element to have no substantial
        // *following* siblings, and both fixtures lead with a `div` so that
        // rule 1 (no substantial *previous* siblings) does not also fire and
        // mask which rule did the work.
        //
        // Rule 4 names `p` and is not conditioned on being blank.
        let (document, standards) = pair(
            "<body><table><tr><td><div>lead</div><p id=cell-p>text</p></td></tr></table></body>",
        );
        assert_eq!(
            margins(&document, "#cell-p").1,
            "0px",
            "§15.3.9 rule 4 zeroes a cell's last paragraph block-end margin"
        );
        assert_eq!(
            margins(&document, "#cell-p").0,
            margins(&standards, "#cell-p").0,
            "rule 4 is not conditioned on having no substantial previous siblings"
        );

        // Rule 3 names the whole default-margin set and is conditioned on the
        // element being blank, so a leading block-end margin survives.
        let (document, standards) = pair(
            "<body><table><tr><td><div>lead</div><ul id=cell-list></ul></td></tr></table></body>",
        );
        assert_eq!(
            margins(&document, "#cell-list").0,
            "0px",
            "§15.3.9 rule 3 zeroes a cell's last blank default-margin element"
        );
        assert_eq!(
            margins(&document, "#cell-list").1,
            margins(&standards, "#cell-list").1,
            "a cell's last default-margin element keeps its block-end margin unless blank"
        );

        // A cell's first *and* blank default-margin element takes both, from
        // rule 1 and rule 2 together.
        let (document, standards) =
            pair("<body><table><tr><td><p id=cell-blank></p></td></tr></table></body>");
        assert_eq!(
            margins(&document, "#cell-blank"),
            ("0px".to_owned(), "0px".to_owned())
        );
        assert_ne!(
            margins(&document, "#cell-blank").1,
            margins(&standards, "#cell-blank").1
        );

        // `figure` has a user-agent margin in this sheet and is deliberately
        // not in §15.3.9's list, so quirks mode leaves it alone.
        let (document, standards) = pair("<body><figure id=fig>x</figure></body>");
        assert_eq!(margins(&document, "#fig"), margins(&standards, "#fig"));
    }

    /// §15.3.8 Tables, "In quirks mode": a cell with `nowrap` and a `width`
    /// that parses as a length takes `white-space: normal`. A percentage width
    /// does not qualify, which is the case the specification singles out.
    #[test]
    fn quirks_mode_normalises_white_space_on_a_sized_nowrap_cell() {
        let markup = "<body><table><tr>\
             <td id=length nowrap width=100>length</td>\
             <td id=percent nowrap width='50%'>percent</td>\
             </tr></table></body>";
        let quirks = Document::parse(markup);
        let standards = Document::parse(&format!("<!doctype html>{markup}"));

        assert_eq!(
            computed_css(
                &quirks,
                &quirks.render_reference(DocumentRenderOptions::default()),
                "#length",
                "white-space"
            ),
            "normal"
        );
        assert_eq!(
            computed_css(
                &standards,
                &standards.render_reference(DocumentRenderOptions::default()),
                "#length",
                "white-space"
            ),
            "nowrap",
            "standards mode keeps td[nowrap]"
        );
        assert_eq!(
            computed_css(
                &quirks,
                &quirks.render_reference(DocumentRenderOptions::default()),
                "#percent",
                "white-space"
            ),
            "nowrap",
            "a percentage width is not a length, so the rule does not apply"
        );
    }

    /// §15.3.3 Flow content, "In quirks mode": a form's block-end margin.
    #[test]
    fn quirks_mode_gives_a_form_a_block_end_margin() {
        let markup = "<body><form id=target><input></form></body>";
        let quirks = Document::parse(markup);
        let standards = Document::parse(&format!("<!doctype html>{markup}"));

        assert_eq!(
            typed_css(
                &quirks,
                &quirks.render_reference(DocumentRenderOptions::default()),
                "#target",
                "margin-bottom"
            ),
            "1em"
        );
        assert_eq!(
            typed_css(
                &standards,
                &standards.render_reference(DocumentRenderOptions::default()),
                "#target",
                "margin-bottom"
            ),
            "0px"
        );
    }

    #[test]
    fn style_discovery_and_quirks_mode_have_explicit_diagnostics() {
        let document = Document::parse("<style>p { color: red }</style><p id=target></p>");
        let options = DocumentRenderOptions {
            document_limits: DocumentLimits {
                max_style_discovery_nodes: 1,
                ..DocumentLimits::default()
            },
            ..DocumentRenderOptions::default()
        };
        let render = document.render_reference(options);

        assert!(render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::StyleDiscoveryNodeLimit
        }));
        assert!(render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::QuirksModeUnsupported
        }));
        assert_eq!(
            typed_css(&document, &render, "#target", "color"),
            "canvastext"
        );
    }

    #[test]
    fn rerender_consumes_a_new_dom_revision_without_html_reparse() {
        let mut document = Document::parse(
            "<!doctype html><style>p { display:block }</style><p id=message>before</p>",
        );
        let first = document.render_reference(DocumentRenderOptions::default());
        let selector = parse_selector_list("#message").expect("selector must parse");
        let paragraph = select_all(
            document.dom(),
            document.dom().document(),
            &selector,
            &MatchContext::default(),
        )[0];
        let text = document
            .dom()
            .children(paragraph)
            .expect("paragraph children")
            .iter()
            .copied()
            .find(|node| {
                matches!(
                    document.dom().node(*node).map(crate::dom::Node::kind),
                    Some(NodeKind::Text(_))
                )
            })
            .expect("paragraph text");
        document
            .dom_mut()
            .set_character_data(text, "after mutation")
            .expect("text mutation must succeed");

        let second = document.render_reference(DocumentRenderOptions::default());
        let diff = second.display.list.diff(&first.display.list);
        assert!(second.revision > first.revision);
        assert_eq!(second.revision, document.dom().revision());
        assert!(!diff.full_repaint);
        assert!(!diff.changed.is_empty() || !diff.inserted.is_empty());
        assert!(!diff.dirty_rects.is_empty());
    }

    #[test]
    fn scroll_extent_and_offset_do_not_change_the_layout_viewport() {
        let document = Document::parse(
            "<!doctype html><style>\
             html, body, div { display:block; margin-top:0; margin-right:0; margin-bottom:0; margin-left:0 }\
             .first { height:10px; background-color:#ff0000 }\
             .second { height:10px; background-color:#0000ff }\
             .third { height:10px; background-color:#00ff00 }\
             </style><body><div class=first></div><div class=second></div><div class=third></div></body>",
        );
        let base = DocumentRenderOptions {
            layout: crate::layout::LayoutOptions {
                viewport: PhysicalSize {
                    width: 8.0,
                    height: 10.0,
                },
                ..crate::layout::LayoutOptions::default()
            },
            raster_background: Color::WHITE,
            ..DocumentRenderOptions::default()
        };
        let top = document.render_reference(base);
        let scrolled = document.render_reference(DocumentRenderOptions {
            scroll_offset: PhysicalPoint { x: 0.0, y: 10.0 },
            ..base
        });

        assert_eq!(top.layout, scrolled.layout);
        assert!((top.layout.fragments.viewport.height - 10.0).abs() < f32::EPSILON);
        assert!(top.layout.fragments.scrollable_content_size.height >= 30.0);
        assert!((scrolled.paint_viewport_origin.y - 10.0).abs() < f32::EPSILON);
        assert_eq!(top.raster.surface.pixel(0, 0), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            scrolled.raster.surface.pixel(0, 0),
            Some(Color::rgb(0, 0, 255))
        );
        assert_eq!(scrolled.raster.surface.height(), 10);
    }
}
