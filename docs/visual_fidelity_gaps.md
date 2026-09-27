# Visual Fidelity Gaps

Verified capability gaps that make real pages render as unstyled plain text.
Every entry below was confirmed by reading the code, not inferred from a report.
Line numbers are the state as of 2026-09-27 (commit `2bd8e8c` plus in-flight work).

This file is a companion to `docs/generic-browser-todo.md`. That file states the
law; this file states the evidence. Per the "Gaps Are Implemented Forward" section,
nothing listed here may be deleted, stubbed, or routed around - each one is a TODO
to implement, or a blocker to report precisely.

## Closed

Kept so the work is not repeated, and so the cost of the fix stays visible.

| Gap | Closed by | Evidence |
| --- | --- | --- |
| **CSS 2.1 §17 table layout was entirely absent.** `tree.rs` produced `FormattingContextKind::Table` but the solver mapped every non-Flex/Grid context to `Block`, so every `display: table` page laid out as one stacked column. | `crates/render-layout/src/solver/table.rs`, in four reviewed rounds | `cargo test -p render-layout`: 81 → 101 → 112 → 119 → **126 passed, 0 failed**. Covers anonymous table boxes, §17.5.2 column widths, §17.5.3 row heights, cell and row-group `vertical-align`, colspan/rowspan, §17.4 caption, §17.5.1 `<col>`/`<colgroup>` widths, §17.5.2.1 `table-layout: fixed`, §17.6 `border-collapse: collapse` with conflict resolution and `hidden` override, §17.5.1.1 `empty-cells` (correctly ignored under collapse, as the spec restricts it to the separated model), shrink-to-fit used width, and over-constrained growth. |

### Table layout verification notes worth keeping

Two things were checked specifically because they are the regressions most likely to be
missed, and both came back clean:

- **The common case did not move.** After row-group alignment, border collapsing,
  `empty-cells` and the intrinsic-width work all landed, the Hacker-News-shaped fixture
  geometry is byte-for-byte identical: rank x 0.000 w 39.740, title x 39.740 w 466.948,
  score x 506.688 w 99.351, comments x 606.039 w 158.961, table 765 x 57.60.
- **Multi-row-group tables with a spanning header** were verified with concrete geometry
  rather than assumed: `thead`/`tbody`/`tfoot` stack in document order with `tfoot` last,
  a header `colspan` cell starts exactly at the body's column x and its right edge equals
  the last spanned column's, and differing group heights do not collapse.

### One accepted deviation in the table layout, and what it would cost to remove

§17.6.2 splits a collapsed border at the grid line, half to each side. The implementation
gives the **whole** resolved border to the winning side and zero to the loser. The reason
is in the paint path, not a shortcut: the rasteriser resolves border colour and style
from the element's *computed style* rather than from the fragment, so a half-and-half
split would paint two different colours along one shared edge - including for `double`.

Winner-takes-all keeps the total occupied space correct, which is what column widths and
content offsets depend on, and paints uniformly. The residual difference from the letter
of the spec is sub-pixel: the winning cell's content is inset by the full border rather
than by half.

Removing it needs one thing: the per-side conflict result - width, style, colour -
carried on the fragment, so the display-list builder can paint a shared edge entirely from
the winner. `BoxGeometry` has no slot for it today. That is a `render-core` change, and it
is recorded as a dependency rather than papered over with a placeholder field.
| **`max_content_width` summed the lines of an inline sequence** instead of taking the widest, so `<br>`-separated content measured too wide. | shared `min_content_width` / `inline_sequence_max_content_width` / `is_forced_break` in `solver/inline.rs`; `intrinsic_flex_size` in `solver/flex.rs` | Four consumers measured `"aaaaaaaaaa<br>bb"` at 96 → **80**: table cell, shrink-to-fit absolute block, `inline-block`, and flex item with `flex-basis: content`. |
| **`is_out_of_flow` keyed off `style_source`**, so an anonymous block inside an absolutely positioned box was classed as positioned and contributed 0 to the parent's height - every auto-height `position: absolute` block reported height 0. | `solver/block.rs`, now following the source element like `float_side` already did | Found while writing the shrink-to-fit test; a general bug, not a table one. |
| **Per-site branches were enforced only by review** while nine agents worked in parallel. | `tools/check_site_neutrality.py` plus `tools/check_site_neutrality_test.py` | 23-case self-test (13 must-flag, 10 must-pass) all green; 150 engine source files scanned clean; wired into `tools/check.sh`, CI, `CLAUDE.md`, `README.md`, `CONTRIBUTING.md`. Four identifier rules - domain, brand token, bare address, site-selecting env/feature - plus a match-scoped allowlist, so a reserved token on a line cannot excuse a site identifier on the same line. |
| **Transport: silent hangs, and a believed-broken connection pool that was not broken** | `crates/render-net/src/{diagnostics,transport,batch,worker}.rs` (new `diagnostics.rs`) | `cargo test -p render-net`: 45 → 62 → **65 passed**. Every request ends in a reported terminal outcome naming its phase and elapsed time. Connection reuse **was** working: `reuse_probe.rs` shows 3 requests over `gzip` use **1** connection and over `gzip, br` use **3**, because ureq 3.3's brotli reader ends at the decoded stream without draining the length-delimited wire body. Live CDN, release: 104.6ms → 20.5ms → 28.0ms. HTTP/2 rejected for a structural reason - see below. |
| **`ToObject` primitive boxing was absent**, so every `Object.*` static threw on a primitive argument where the spec requires a wrapper. | `crates/render-js/` | `cargo test -p render-js`: 195 → **203 passed**. The 2.27 MB qq.com main bundle now **runs to completion** (273,779 interpreter steps), from a first failure at byte 302625. See the root-cause correction below. |
| **Web Storage was absent entirely** - the single most damaging omission in S10. | `crates/render-js/src/runtime/builtins/storage.rs` (new) | `localStorage` and `sessionStorage` now exist. It was the last thing standing between the qq bundle and completion. |
| **The list of active formatting elements was absent**, so misnested formatting tags still mis-nested and in-body's `select`/`option`/`optgroup`/block arms called a reconstruction step that did not exist. | `crates/render-html/src/tree_builder.rs` | `cargo test -p render-html` 66 → 81 → **98 passed**. Full implementation including the Noah's Ark clause, the four marker sites, the scope-boundary table, reconstruction, and the adoption agency algorithm. The spec's own three worked examples (13.2.10.1/.2/.3) are tests and match their printed trees exactly. |
| **Foster parenting fired on the flag instead of on the target**, so `<table><b>x</table>` put "x" between the `b` and the table. | `foster_parenting_applies_to` in `tree_builder.rs` | Found while implementing 13.2.10.3. Pre-existing, and the adoption agency tests would not have caught it. |

### A root cause that was real, and still not the cause

This one is worth reading twice, because it is a failure mode that recurs.

The work order said: `Object.getPrototypeOf` calls `require_object`, which throws for every
non-object primitive, so `Object.getPrototypeOf("x")` throws where the spec requires
`ToObject` - and that is why the qq.com main bundle dies. The first half of that was
**verified and correct**. `require_object`
(`crates/render-js/src/runtime/builtins/dom.rs:1075`) did throw
`"primitive object coercion is not implemented in this runtime slice"` for every
primitive, and every `Object.*` static routed through it.

It was **not** the cause. The `getProto: not an object` throw is emitted by the **bundle's
own** `get-proto` fallback, not by the engine. `get-proto@1.0.1` tries
`Reflect.getPrototypeOf` first and only falls back to a stricter `Object.getPrototypeOf`
wrapper when `Reflect.getPrototypeOf` is **absent** - and it was absent. The engine's own
`require_object` bug sat on that same path but was **masked by the bundle's guard**.

The probe measured it: fixing `to_object` alone moved the first failure by **zero bytes**.
The real blockers, in order, were `Reflect`, then `String.prototype[Symbol.iterator]`, then
`Object.prototype.__proto__`, then Web Storage.

So: **verifying that a bug is real is not the same as verifying that it is the cause.** The
first is a code-reading fact; the second needs a measurement that can come back zero. An
agent that reports "my fix contributed nothing" has done the most useful thing available,
and the report should be believed - the alternative is confidently attributing a page
failure to a defect that had nothing to do with it.

### Why HTTP/2 was rejected, and one premise that was wrong

Not "the codec is missing" - the reason is structural, and it is worth recording because
it will come up again. A pooled ureq `Connection` is *removed* from the pool while in use
and returned by `reuse()`, and `run()` holds `&mut Connection` for the whole request: one
in-flight request per connection. Multiplexing is the exact inverse, so it cannot be
expressed inside ureq's `Connector`/`Transport`/pool model. It would need a bespoke client
or a different HTTP stack, either of which risks the system-proxy path, the rustls config,
the pooling above, and the connect budget. ureq 3.3 also has no ALPN API at all, so no ALPN
extension is sent and RFC 7301 lets the server choose.

A working HTTP/1.1 path is worth more than a broken HTTP/2 path, so the decision was not
to build it.

The premise that was wrong: this machine's `curl` is built `Schannel zlib` with **no HTTP2
feature**, so the earlier "curl came back in 46ms on the second hit" was HTTP/1.1 to
HTTP/1.1 - the same protocol the engine speaks. It was never an HTTP/2 comparison.
| **S5: inline SVG and MathML were not parsed as foreign content.** `render-html`'s tree builder had no occurrence of `svg`, `MathML`, foreign content, or integration points, so an inline `<svg>` became an unknown HTML element and its children got no geometry. `render-dom` was already namespace-ready, so the gap was confined to the tree builder. | `crates/render-html/src/tree_builder.rs` and `tokenizer.rs`, plus `set_attribute_ns`/`attribute_ns` in `render-dom` | `cargo test -p render-html` 31 → **66 passed**; `render-dom` 12 → **14**. Implements the §13.2.6 dispatcher, the §13.2.6.5 in-foreign-content rules including the 44-name breakout list, the three adjustment tables in full (37 SVG tag names, 58 SVG attributes, 11 foreign attributes), MathML text and HTML integration points, and CDATA sections. Every table entry is asserted as data rather than spot-checked. |
| **S2: the UA stylesheet was 28 lines long.** `h1`-`h6` got `display: block` and nothing else, so every heading rendered identically to body text, and there was no `a` rule at all, so links were visually indistinguishable from plain text. | `crates/render-core/src/document.rs`, `UA_STYLE_SHEET` | Now a section-annotated implementation of the WHATWG HTML rendering section: §15.3.1 hidden elements, §15.3.2 the page, §15.3.3 flow content, §15.3.4 phrasing content, §15.3.6 sections and headings with the full `h1`-`h6` scale, §15.3.7 lists, and further sections below. `a:link` and `a:visited` carry colour and `text-decoration-line: underline`; `b`/`strong` are `bolder`; `code`/`kbd`/`samp`/`tt` are monospace; `del`/`s`/`strike` are line-through. |
| **S3: twelve paint-relevant properties had zero consumers** - `text-decoration*`, `text-shadow`, `list-style*`, `text-overflow`, `text-indent`, `letter-spacing`, `text-transform`, `filter`, `clip-path`, `backdrop-filter`, `mask-image`, `mix-blend-mode`. | `crates/render-core/src/paint/` | `text-decoration`, `text-shadow` and list markers now have producers and rasterisers: `paint_text_decoration`, `paint_text_shadow` (with a two-dimensional Gaussian approximating the CSS blur, documented at the impl) and `paint_list_marker` with `ListMarkerShape::{Disc, Circle, Square}`, plus `DisplayCommand::ListMarker`. **Still open within this row:** `text-overflow`, `text-indent`, `letter-spacing`, `text-transform` need inline layout, and `filter`, `clip-path`, `backdrop-filter`, `mask-image`, `mix-blend-mode` are new rasteriser capabilities. |
| **S12: only `rgb()` was supported.** Every `hsl()`, `oklch()`, `lab()`, `hwb()`, `color()` and `color-mix()` declaration was invalid at parse time and dropped. | `crates/render-css/src/properties.rs` | Declarations dropped at computed-value time across the `.diag` corpus: **58 → 11**. `hsla()` alone: 38 → 0. `hsl()`/`hsla()` resolve to sRGB per CSS Color 4 §4.3/§4.4 in both syntaxes; `background-image` takes a `<bg-image>#` list. `cargo test -p render-css`: **91 passed, 0 failed**. |
| **S15: real-world selector parse failures.** A comment acted as a combinator, so `.a/*c*/.b` parsed as a descendant selector - over-matching as well as mis-parsing. Declaration values also kept leading comments, and `background-image` accepted only one layer. | `crates/render-css/src/{selector,stylesheet,properties}.rs` | `cargo run --release -p render-css --example css_corpus` reports **0 dropped rules of 11 194** across 1.93 MB of real CSS. The 134 remaining diagnostics are the IE star hack, which CSS Syntax §5.4.4 requires a browser to drop too. |

### A caveat on the UA stylesheet's dynamic rules

The new sheet contains `a:visited`, and that rule is inert: `:visited` reads
`MatchContext::visited_links` (`render-css/src/selector.rs:1330`) and that set is never
populated. See S16. This is a missing feature that degrades quietly rather than a
page-breaking defect - a browser with no history also shows nothing visited.

`a:link`, by contrast, **does** work, and it carries most of the value here. `:link` is
matched structurally at `render-css/src/selector.rs:1326-1329` - an `a`, `area` or `link`
element with an `href` attribute - with no dependence on engine state. So link colour and
underline work today.

### Two corrections the implementation made to the task brief

Recorded because they are the kind of detail that is easy to get wrong and worth
getting right. The WHATWG HTML "adjust foreign attributes" table has **11** entries and
does **not** include `xml:base`; and `xmlns:xlink` maps to the **XMLNS** namespace
(`http://www.w3.org/2000/xmlns/`) with prefix `xmlns` and local name `xlink`, not to the
XLink namespace. Both were wrong in the brief that asked for the work.

### One latent bug the parsing work exposed

`crates/render-html/src/serialization.rs` skipped any attribute carrying a namespace or
prefix. That was harmless while nothing produced such attributes, and would have
silently discarded every `xlink:href` the new parser emits. It now restores the
qualified name, with a round-trip test. This is the general hazard: a filter written when
a data shape does not yet exist becomes a silent data loss the moment it does.

## How the list was produced

Two questions, asked of the whole workspace:

1. Which CSS properties reach a consumer in layout or paint at all?
2. Which parsed or computed values stop travelling and are dropped on the way?

A property that is parsed into the computed style map but read by nobody is inert.
That class of defect is invisible in the logs, which is why pages look wrong with no
diagnostic explaining it.

---

## S1 - The typographic axis is structurally missing (highest impact)

`crates/render-layout/src/solver/mod.rs:33`

```rust
pub struct TextStyle {
    pub font_size: f32,
    pub line_height: f32,
}
```

**Scope correction, found later:** this is a **three**-crate change, not two. S1 originally
named `render-layout` and `render-browser`. It is also `render-core`, because
`crates/render-core/src/interaction/hit_test.rs:903` builds `TextStyle` as a struct literal
in production code, so adding a field breaks it.
`crates/render-browser/src/font_backend.rs` only *reads* fields, so it is unaffected by the
field addition itself.

A second, smaller finding from the same round: the letter- and word-spacing work needed a
place to carry its values, and could not extend `TextStyle` for the reason above. It went
around the problem in a way worth keeping - a sibling `TextSpacing` type, plus a
**defaulted** `TextMeasurer::measure_spaced` whose default computes spec-correct advances.
`font_backend` implements only `measure`, so it inherits the default and gets correct
tracking with no change in that crate. That is the right shape for the S1 change to follow:
a defaulted method extends the seam without breaking the one implementor.

There is no `font_weight`, no `font_style`, no `font_family`. The data reaches the
style map and is then dropped one metre from the glyphs:

- `crates/render-css/src/cascade.rs:833-838` writes `font-style`, `font-weight` and
  `font-family` into the text style handed to the measurer/shaper/painter.
- `crates/render-layout`'s `TextStyle` has no field to receive them.
- `crates/render-browser/src/font_backend.rs:33-43` loads exactly **one** font per
  fallback candidate group (it `break`s on the first success) and
  `font_backend.rs:57-63` picks a font purely by glyph coverage, ignoring weight and
  style entirely.

Consequences, all of them unconditional:

- **Bold is physically impossible.** `<b>`, `<strong>`, `font-weight: 700` and every
  heading render at regular weight.
- **Italic is physically impossible.**
- **`font-family` is ignored.** A page asking for `PingFang SC` / `Microsoft YaHei` /
  `sans-serif` gets whichever font the backend happened to load first.

Fixing this requires `TextStyle` to carry the three values, the `TextMeasurer` /
`TextShaper` / `TextPainter` traits to pass them, and the backend to select a face by
`(family, weight, style)` instead of coverage alone. It spans `render-layout` and
`render-browser`, so it is sequenced after the agents that own those crates.

## S2 and S3 - closed

The 28-line UA stylesheet and the missing paint consumers are both implemented; see the
Closed table for the evidence and for what remains open inside S3.

One thing worth keeping from the original S3 write-up, because it is the method that
found the gap and it still applies to whatever is left: these properties did have
working consumers elsewhere, which is what made the absence misleading rather than
obvious. `box-shadow`, `object-fit` and `contain` are consumed in
`crates/render-core/src/paint/display_list.rs`; `font-size`, `line-height` and
`white-space` in `crates/render-layout/src/solver/inline.rs`; `vertical-align` in
`crates/render-layout/src/solver/table.rs`. A property being in that list is the only
evidence that it works - the absence of a diagnostic is not evidence that it does not.

## S16 - Dynamic pseudo-class state is never populated

This one is a complete capability seam that is simply not wired, which makes it cheap
relative to its effect.

The selector engine has the state. `MatchContext`
(`crates/render-css/src/selector.rs:285-293`) carries `focused`, `target`, `hovered`,
`active` and `visited_links`, and the matcher reads all of them - `:hover` at `:1349`,
`:visited` at `:1330`, `:focus`/`:focus-visible` at `:1342`.

Nothing ever fills them in. The page pipeline constructs the context with viewport
dimensions only:

```rust
// crates/render-core/src/document.rs:808-812
&MatchContext {
    viewport_width: Some(options.layout.viewport.width),
    viewport_height: Some(options.layout.viewport.height),
    ..MatchContext::default()
},
```

So on a real page `:hover`, `:active`, `:focus`, `:focus-visible`, `:focus-within`,
`:target` and `:visited` never match. Consequences: no hover-revealed dropdown or
flyout, no hover underline or button state, no focus ring for keyboard navigation, and
no fragment-target highlight.

Note the history here, because it is a trap: an older report claimed these pseudo-classes
*always* matched, which was wrong in the opposite direction and made every `:hover` style
show permanently. They now never match without state, which is the correct default but
still not the correct behaviour. The fix is not in the selector engine at all:

- `DocumentRenderOptions` (or an equivalent seam) must accept the interaction state.
- `render-browser` must derive `hovered` from the pointer position - it already has hit
  testing in `crates/render-core/src/interaction/hit_test.rs` - and set `focused` from
  keyboard focus.
- A state change must schedule a style/layout/paint invalidation, which
  `crates/render-core/src/invalidation.rs` exists to do.

`:link` is explicitly **not** affected: it is matched structurally from the tag name and
the `href` attribute, so link colour and underline already work.

## S4 - `@font-face` and `@keyframes` are parsed and discarded

`crates/render-css/src/stylesheet.rs` handles `ParsedRule::KeyframesBlock` and
`ParsedRule::FontFaceBlock` by dropping them. Grepping `crates/render-core` for
`@font-face`, `keyframes`, `animation` or `transition` returns nothing.

Counted on the real corpus by `css_corpus`: **223 `@font-face` blocks and 249
`@keyframes` blocks** are parsed and thrown away.

Consequences: no webfont on any site (nearly every Chinese site self-hosts 阿里巴巴普惠体,
思源黑体, HarmonyOS Sans and friends), and no CSS animation or transition anywhere.

Correctly **not** done as a half-feature: a parse-only `StyleSheet::font_faces` struct
that nothing can consume would satisfy the parser and change no rendering, which the
project's "implement features fully or not at all" rule forbids. The real work needs
`render-net` (font file fetching), face registration and glyph rasterisation, and an
animation clock.

## S4b - `@supports` is never evaluated

All **93** `@supports` blocks in the corpus are applied **unconditionally** - the
condition is parsed and then ignored, with a comment in `stylesheet.rs` acknowledging it
only "honors the common `@supports (display: grid)` blocks". On this corpus the effect is
benign (all three distinct queries are positive, one with `or`), which is exactly why it
is dangerous: it looks correct until a site uses `@supports not (...)` to provide a
fallback.

A correct implementation needs a property-support oracle - the same question the
`spec/registry.rs` capability register is nominally about, and which S9 says is not
tracking these gaps.

## S5 - Closed, see "S5 - Closed: inline SVG parses and rasterises" below

Superseded. The parsing half landed earlier and the rendering half has now landed too, so
this entry is kept only as a pointer. The `use`/`symbol`/`defs` and `foreignObject`
omissions recorded here are still accurate.

The parsing half is closed - see the Closed table. The rendering half is not, and it is
worth stating precisely what is missing, because "inline SVG is supported" is now half
true and that is an easy thing to be wrong about.

- `render-layout` and `render-core` produce **zero geometry** for foreign content.
- An `<svg>` element's size must come from its `width`/`height`/`viewBox` attributes,
  because layout cannot measure it. No replaced-element sizing path exists for it.
- The available seam is good: the DOM is now faithful (case-adjusted local names such as
  `clipPath`, `foreignObject`, `viewBox`; namespaced attributes preserved as
  `xlink:href`, `xml:lang`, `xmlns:xlink`), `render_html::serialize_html_node` is
  namespace-aware, and `crates/render-core/src/image/svg.rs` already rasterises standalone
  SVG. So the intended path is: detect an `svg` element in the `Svg` namespace, serialise
  its subtree, feed it to that rasteriser, and register the result as an image resource so
  replaced-element sizing works. Roughly a hundred lines and no new rendering code.
- Read namespaced attributes with `attribute_ns(node, Some("http://www.w3.org/1999/xlink"), "href")`.
  `attribute(node, "href")` deliberately returns `None` for `xlink:href`, because that
  attribute is in a different namespace from a null-namespace `href`.

## S14 - Closed: `<template>` contents are now inert

Was: `<template>` fell through the generic "any other start tag" arm, so
`<template><tr><td>x</td></tr></template>` produced `template > tr > td` as **live**
document elements - scriptable and queryable when the spec says inert.

`create_element("template")` now allocates a `DocumentFragment` and stores its id in
`ElementData.template_contents`. The fragment is **never appended to the template**, so it
has no parent and the template has no children.

The inertness is therefore **structural, not a filter**, which is the part that matters.
Both document traversals were checked rather than assumed:

- `find_element_by_id` in `crates/render-js/src/runtime/builtins/dom.rs:979-992` starts at
  `dom.document()` and walks only `dom.children(node)`.
- `collect_matches` in `crates/render-css/src/selector.rs:370-375` recurses only via
  `dom.children(root)`.

A parentless fragment is not a child of anything reachable from the document, so neither
can reach it. A traversal added later cannot bypass this either, because the invariant is
"fragments are parentless" - a structural property of the DOM, not a rule a new code path
would have to remember.

`cargo test -p render-html` 66 → **81 passed**; `render-dom` 14 → **16**. All 10
pre-existing table, foster-parenting, caption, colgroup and list tests unmodified.

## S19 - Insertion modes: the audit itself was wrong, and one root-cause gap is closed

### The correction that matters

An earlier version of this entry listed `in select` and `in select in table` as missing
insertion modes, and a work order was written to implement them. **Both were wrong: those
two modes do not exist in the current standard.** The WHATWG HTML Living Standard
(Last Updated 25 September 2026) defines 21 insertion modes at 13.2.6.4.1 through .21, and
the select rules were folded into "in body" (13.2.6.4.7). The string "in select" does not
appear as a mode name in that document.

Implementing them would have **introduced a spec violation**, and the agent doing the work
refused, verified against the fetched spec, and implemented the current `select` /
`option` / `optgroup` / `input` rules from 13.2.6.4.7 instead. The stray-markup shape is now
pinned by a test that says explicitly what the spec requires, rather than by a deleted mode.

This is the third time in one session that an agent corrected a spec detail I had supplied
and the agent turned out to be right (`xml:base` not being in the 11-entry foreign-attribute
table, the `xmlns:xlink` mapping, and now this). See the working rule at the end of this
file.

### Where the modes actually stand

Of the 21, the builder now implements 17. The four still missing:

- **`in head noscript`** (13.2.6.4.5) - genuinely reachable. With scripting enabled the
  content of a `<noscript>` is raw text and must not become live DOM. The body case is
  masked downstream, because the UA sheet has `noscript { display: none }`. The `<head>`
  case is the problem: it produces `head > noscript > style > #text`, i.e. a `<style>` that
  a scripting-enabled browser would have tokenized as text and ignored - so a stylesheet
  gets applied that a browser would not apply. Deliberately held back until someone can
  point at a measured real-page regression.
- **`in frameset`**, **`after frameset`**, **`after after frameset`** - reachable only via
  `<frameset>`, which no live page uses. Recorded as a permanent, deliberate gap.

### Closed: the list of active formatting elements

This was called "a bigger win than any single missing mode", and it was right. The spec's
"reconstruct the active formatting elements" step is called by in-body's `select`,
`option`, `optgroup` and block arms; without the list, misnested formatting tags still
mis-nested.

Implemented in full: the list with markers, the push button with the Noah's Ark clause
(three per family after the last marker, compared on tag name, namespace and attributes as
an unordered multiset), the four marker sites (`template`, `applet`/`marquee`/`object`,
`td`/`th`, `caption`) and all four clear-to-last-marker sites, the scope-boundary table,
reconstruction, and the **adoption agency algorithm**. `render-dom` needed no change: the
list is parser state that references elements by node, and the adoption agency's one
non-obvious step is exactly what `Dom::append_child` already does.

**The spec's own three worked examples are now tests and match their printed trees exactly**
- 13.2.10.1, 13.2.10.2, and 13.2.10.3. Before and after:

| input | before | after |
| --- | --- | --- |
| `<p>1<b>2<i>3</b>4</i>5</p>` | 3 flat formatting siblings, text at the wrong level | `p>1 b>2 i>3`, then `i>4`, then `5` |
| `<b>1<p>2</b>3</p>` | `b>1 p>b>b>b>b>b>b>2` with `3` inside the `p` | `b>1`, then `p>b>2, 3` |
| `<table><b><tr><td>aaa</td></tr>bbb</table>ccc` | `b` inside the `td` | `b`, `b>bbb`, `table>…>td>aaa`, `b>ccc` |

### Also closed: `in table text`, and a real foster-parenting bug it exposed

`in table text` (13.2.6.4.10) is implemented with the pending-token list and the group
reprocess. The mixed-whitespace path now behaves: `<table> &amp; x <tr>` yields one
foster-parented text node `" & x "` before the table with nothing left inside it. Before,
the leading space went into the table and only the rest was foster parented.

While implementing 13.2.10.3 it found a pre-existing bug: foster parenting was applied
whenever the *flag* was set rather than when the insertion *target* was a
table/tbody/tfoot/thead/tr element, which put `<table><b>x</table>`'s "x" between the `b`
and the table. Fixed by testing the target, as 13.2.6.1 specifies.

## S20 - Same-origin concurrency is set by a third-party default, not by us

Found while verifying connection reuse, and it is a capacity limit rather than a
correctness bug, so it is easy to miss.

- `max_idle_connections_per_host` in the ureq agent configuration defaults to **3**. A
  single page pulling 40 assets from one origin over HTTP/1.1 - the common case for a
  Chinese portal, and the shape of every corpus in `.diag/` - has three idle slots to reuse
  and must serialise the rest behind fresh handshakes.
- ureq's `Connection::age()` always returns `0`, so `max_idle_age` never evicts anything.
  The setting is inert rather than wrong.

Neither is a bug in this repository, but both mean the same-origin concurrency ceiling is
decided by a dependency's default rather than by a decision anyone here made. Now that
connection reuse is proven to work - see the Closed table - that ceiling is the binding
constraint on parallel asset loading, and it should be an explicit, measured choice. Raise
it, measure the effect on a real page, and record the number.

## S15 - Closed: the real-world selector parse failures are gone

Was: `.diag/qq/GAP_REPORT.md` recorded `stylesheet Warning CssSyntax expected a selector
combinator` at byte 25, 53 and 58 of qq's real `index.css`, so some rules were dropped
entirely. The cause was not `:is()`/`:where()`/`:not()`/`:has()` - those were already
implemented.

Cause: `skip_whitespace_and_comments` reported "I consumed something", so `.a/*c*/.b` was
read as a **descendant** combinator. Comments are deleted during preprocessing per CSS
Syntax §4.3.1, and a descendant combinator is real `<whitespace>` per Selectors 4 §4.2.
That made selectors *over*-match as well as mis-parse. Fixed, with the negative case
now locked by a test. The `::view-transition-new/old(root)` selectors in qq's sheet are
handled.

`css_corpus` now reports **0 dropped rules out of 11 194** across 1.93 MB of real CSS.
The 134 remaining diagnostics are the obsolete IE star hack (`*display:inline`), which
CSS Syntax §5.4.4 says a browser must also drop - so dropping them is correct.

The measurement tool found two more genuine defects while being built: declaration
values were keeping leading comments, so `background: /*x*/ blue` failed to classify; and
`background-image` accepted only one layer. Both fixed.

**The measurement gap this exposed, which matters more than any single fix:**
`css_corpus` originally counted only **rule-level** drops. The 358 dropped colour
declarations of S12 were invisible to it - they were not rules being dropped, they were
declarations inside surviving rules. A gap report that counts the wrong unit will report
zero problems while hundreds of declarations are being discarded. It now diffs
declaration-level acceptance too.

## S6 - Parsed but not implemented

| Capability | Evidence |
| --- | --- |
| `position: sticky` | `render-css/src/properties.rs:758` defines the keyword; no consumer anywhere. Pinned headers scroll away. |
| `z-index` | Read as raw text at `render-layout/src/solver/mod.rs:399` and used to sort block siblings at `solver/block.rs:747`. Flex and grid children are not sorted, the paint layer has no z-index awareness, and only `transform`/`opacity` create a stacking context. |
| `text-indent`, `letter-spacing`, `text-transform` | Initial values only (`render-css/src/computed.rs:77,80,81`); no consumer. |
| Multi-column layout | No `column-count`/`column-width` support. |

## S7 - Quirks mode, see "S7 - Quirks mode: parse side done, behaviour not started" below

Superseded. The parse side and the selector-matching side are now wired; the CSS 2.1 §9.2.1.1
behaviour is still unstarted, and the correction that `MatchContext.quirks_mode` is **not**
inert is recorded in the entry below.

`crates/render-core/src/document.rs:704` emits
`"{} CSS quirks are not implemented; standards-mode CSS semantics were used"`. Any page
served without a doctype - which is most of the older web - is laid out with
standards-mode box model rules and therefore wrong.

## S8 - Video stops at the bitstream layer

This needs a precise statement, because the code is further along than the older
handoff notes suggest.

Done: `crates/render-js/src/video/demuxer.rs` and `crates/render-js/src/video/avc.rs`
implement the container and bitstream layers - MP4 sample description parsing,
`avcC` record parsing (`parse_avc_decoder_config`), NAL unit classification, SPS
parsing to recover the coded picture size behind `videoWidth`/`videoHeight`,
emulation-prevention byte stripping, and Annex-B conversion.

Not done: entropy decoding and reconstruction. `avc.rs` says so itself - "Pixel
decoding itself lives behind the `VideoDecoder` trait". The shipped default in
`crates/render-js/src/video/mod.rs:242` is still `PlaceholderDecoder`, which always
returns `VideoError::DecoderUnavailable`, so no site ever presents a decoded frame.
Tests inject a synthetic `ColorTestDecoder`, so the bitstream path is exercised but
real playback is not reachable.

Consequence: `<video>` shows its poster and nothing else. Bilibili, and every other
video site, renders a static poster.

## S9 - The capability registry does not track these gaps

`crates/render-core/src/spec/registry.rs` defines 20 `FeatureDefinition` entries, all
with `SupportStatus::Partial`, and has no entry at all for animations/transitions,
`@font-face`, pseudo-elements, table layout, inline SVG, video decode, or forms. A gap
that is not registered cannot be tracked, and cannot be reported as progress either.

## S10 - The platform surface is missing the APIs real pages touch first

Enumerated from the 117 globals registered in `crates/render-js/src/value.rs`.

Present: `Proxy`, `Reflect`, `URLSearchParams`, `navigator`, `location`, `history`,
`screen`, `IntersectionObserver`, `MutationObserver`, `queueMicrotask`, `atob`, `btoa`,
`Blob`, `fetch`/XHR, and - as of the ToObject round - **`localStorage` and
`sessionStorage`**, which were the worst omission in this table.

Absent:

| Missing | Why it matters |
| --- | --- |
| ~~`localStorage`, `sessionStorage`~~ **now implemented** | Was the top item: read at startup by a large share of production bundles and by essentially every analytics and anti-bot script, so a missing global threw a `ReferenceError` and killed the page's main script before it rendered anything. It was the last blocker standing between the qq.com main bundle and completing. |
| `FormData` | Every HTML form submission path and every `fetch`/`XHR` call that sends a body. |
| `TextEncoder`, `TextDecoder` | Used pervasively by bundled base64/utf8 helpers. |
| `structuredClone` | Used by current framework state handling. |
| `AbortController`, `AbortSignal` | Used by essentially every `fetch` wrapper. |
| `ResizeObserver` | Component frameworks gate layout on it. |
| `customElements`, `attachShadow`, `ShadowRoot` | Custom elements and shadow DOM; `docs/generic-browser-todo.md` Priority 3. |
| `DataView` | Absent even though the whole `TypedArray` family is implemented in `runtime/builtins/typed_array.rs`. A small inconsistency in an otherwise complete area. |
| `WebSocket`, `Worker`, `SharedWorker`, `EventSource`, `BroadcastChannel`, `MessageChannel` | No live-update or off-main-thread execution path. |
| `indexedDB`, `Notification`, `crypto`, `PerformanceObserver`, `WebAssembly`, `ReadableStream`, `TransformStream`, `WeakRef`, `FinalizationRegistry`, `BigInt`, `Atomics` | Lower frequency on ordinary pages. |

A missing global is worse than a wrong one, because it throws a `ReferenceError` at
the point of use and takes the surrounding script with it. Every remaining entry above is a
candidate to check against real-site script corpora already in `.diag/`.

## S11 - No nested browsing contexts

`iframe` appears in `crates/render-core/src/document.rs:262,269` only as a member of
element classification lists. There is no nested browsing context: no second document,
no separate event loop or task queue, no `postMessage`, no `window.open`. Every
`<iframe>` - embedded video, ad slots, third-party widgets, login frames - renders as
nothing at all. `target="_blank"` and `window.open` are equally absent, so a link that
opens a new context silently does nothing.

## S12 - Closed: only `rgb()` had been supported, and `hsl()` is now implemented

Was: `crates/render-css/src/properties.rs:1685-1689` recognised only `rgb()`/`rgba()` and
named colours, so every `hsl()`, `oklch()`, `lab()`, `hwb()`, `color()` and
`color-mix()` declaration was invalid at parse time and the cascade dropped it.

Measured over the nine real production stylesheets in `.diag/**`, via
`cargo run --release -p render-css --example css_corpus`:

| | before | after |
| --- | ---: | ---: |
| declarations dropped at computed-value time | **58** | **11** |
| of which `hsla()` (`background-color` 23, `color` 15) | 38 | **0** |
| of which `background-image: url(x),none` (single-layer only) | 16 | **0** |
| of which `color(display-p3 …)` | - | 7 |
| of which genuinely invalid CSS a browser also drops | 4 | 4 |

`hsl()`/`hsla()` are parsed in both the legacy comma syntax and the modern space syntax,
with all six hue sectors, four angle units, hue wraparound and channel clamping, and
resolved to sRGB per CSS Color 4 §4.3/§4.4. `background-image` now accepts a
comma-separated `<bg-image>#` list per CSS Backgrounds 3 §3.2.

**Still open in this area:** `color(display-p3 …)` needs a second colour space and gamut
mapping; the agent deliberately kept `CssColor` to the single sRGB variant rather than
half-implement a second space.

## S17 - Closed, see "S17 - Closed: two media-query evaluators" below

Superseded, and **the severity recorded here was wrong**. This entry described the cost as a
false "unsupported" warning. It was not: the flag fed `is_eligible()`, so
`<link media="screen and (min-width:768px)">` was **dropped from the plan and never fetched**.
Responsive stylesheets were not being loaded at all.

This is the same shape of defect as the stale `SrcsetUnsupported` and `document.cookie`
claims: a supported thing is being reported as unsupported, so people chase a non-bug.

There are two independent media-query evaluators:

- **The real one**, `media_query_list_matches` in
  `crates/render-css/src/cascade.rs:256`, used by the cascade at `cascade.rs:124` to
  decide which rules apply. It evaluates media features properly - `width`, `min-width`,
  `max-width`, `min-height`, `max-height` against `MatchContext.viewport_width` and
  `viewport_height` (`cascade.rs:351,372`) - and it handles the real-world form
  `(min-width:1560px)and (max-width:2059.9px)` written with no spaces around `and`
  (`cascade.rs:294-297`, `split_media_conjunctions`). Measured on the corpus: 884 rules
  gated, 129 applying at 1770x1170.
- **The weak one**, in `crates/render-core/src/document.rs:1230-1261`, used to decide
  `media_fully_supported` and to emit `DocumentDiagnosticCode::MediaQueryUnsupported`. It
  only understands the media *type*. Line 1244 is the bug:
  `media_type.filter(|_| words.next().is_none())` marks any query with a feature as
  `has_unsupported_query`, because the next word after `screen` is `and`.

So every `@media (min-width: 768px)` on a real page produces a
`MediaQueryUnsupported` warning while being evaluated correctly. The warning maps to
`StylesheetDiagnosticSeverity::Warning` in
`crates/render-browser/src/resources.rs:526-532`, so there is no behavioural consequence -
but the diagnostic is false, and the project's own law cuts both ways: unsupported CSS
must never be silently ignored, and supported CSS must never be reported as unsupported.

Fix: derive the support flag from what the cascade can actually evaluate, rather than
parsing the query a second time with weaker rules. One evaluator, one answer.

## S18 - `MatchContext` is constructed exhaustively, which blocks three queued items

`crates/render-browser/src/render_worker.rs:497-509` builds the context by naming all ten
fields, with no `..MatchContext::default()`:

```rust
MatchContext {
    scope: None, quirks_mode: ..., pseudo_element: None,
    focused: None, target: None,
    hovered: HashSet::new(), active: HashSet::new(), visited_links: HashSet::new(),
    viewport_width: Some(viewport.width), viewport_height: Some(viewport.height),
}
```

Adding a field to `MatchContext` therefore breaks the build in `render-browser`. That
blocks, in one stroke:

- **S16** - dynamic pseudo-class state. The seams exist and the matcher reads them, but
  the state has to travel from the browser's hit test into the context, and today
  `document.rs:808-812` only supplies viewport dimensions.
- **Device pixel ratio in media queries.** `resolution` and the `*-device-pixel-ratio`
  family evaluate false unconditionally - **82 occurrences in the corpus** - so
  `@media (-webkit-min-device-pixel-ratio: 1)`, which must be *true* on any 1x display,
  drops its rules. This needs `MatchContext.device_pixel_ratio`, and the browser already
  knows the value from its fractional-DPI surface.

The cheapest unblocking move is to add `..MatchContext::default()` at that construction
site so future fields are non-breaking. That is a one-line change in `render-browser`
and it converts three blocked items into unblocked ones.

## S21 - Gaps the ToObject round found and deliberately left

Reported rather than fixed, each with the reason. All are in `render-js`.

- **`Object.prototype.hasOwnProperty.call(obj, key)` answers about the wrong thing.** It
  ignores `this` and reads the key from `arguments[0]`, so the call answers about the
  *key* `obj`. This is a widely used library idiom, so the blast radius is larger than the
  line count suggests. Fixing it means threading call arguments through the whole native
  layer rather than special-casing this one builtin.
- **`String` exotic writes do not throw in strict mode.** `set_member` silently ignores
  `Object('a')[0] = 'b'` in both sloppy and strict code; the spec wants a `TypeError` in
  strict mode. Pre-existing, and the new shared wrapper path makes it harder to split
  sloppy from strict.
- **`require_date_value` never unwraps a primitive.** Unreachable today, because member
  access boxes the receiver first, but `Date.prototype.getTime.call(5)` would throw
  instead of returning `NaN`. Latent, and reachable the moment a call-argument path stops
  boxing.
- **Nullish leniency in `Object.assign`, `defineProperty(s)`, `keys`, `values`, `entries`**
  is a deliberate, documented page-compatibility deviation - the spec throws. Recorded
  here so it is a decision on the record rather than an accident.
- **`Object.getOwnPropertyNames` on a String wrapper is a virtualised view**, not
  materialised slots, so `Object.defineProperty(Object('a'), '0', ...)` throws while
  `Object.defineProperties` on the same key takes the lenient path. Consistent for the
  spec's observable results; materialising was rejected as a heap-exhaustion risk.
- **No `BigInt`** in `JsValue`, so there is no BigInt wrapper to box. `get-intrinsic`
  records `"%BigInt%": undefined` and bundles cope.

## S22 - `ComputedStyle` cannot express "the author did not declare this"

A landmine the parallel work stepped on, and it will be stepped on again.

`ComputedStyle::get(property)` returns `Some(value)` for every property the registry
defines, because the registry installs an initial value for each. So once a property is
registered, **"declared" and "initial" become indistinguishable**: solver code that asks
"did this element say anything about X?" gets a false "yes" for every element.

The live proof is a currently-failing test. Registering `vertical-align` in the property
registry made `crates/render-layout/src/solver/table.rs:287-290` take its wrong arm:

```rust
match style.and_then(|style| style.get("vertical-align")) {
    Some(_) => Self::table_vertical_align(style),      // "the group declared it"
    None => Self::table_vertical_align(table_style),   // "it declared nothing" — now unreachable
}
```

`cargo test -p render-layout` is 154 passed / **1 failed**:
`table_tests::a_row_group_without_its_own_alignment_follows_the_table`, row y 0 where 40.8
is expected. A probe confirmed the mechanism: `table_va=Some("bottom")` but
`group_va=Some("baseline")` - the group's own registered initial value always wins, so the
table's `vertical-align` can never reach its row group.

**This is not one test.** It is an API limitation, and registering the six table properties
is exactly the kind of work that triggers it. Any future solver logic that needs to know
whether a value was *authored* will be silently wrong in the same way, and the failure will
look like a layout bug rather than a cascade bug.

### The inheritance question, settled against the document

An earlier version of this entry claimed the registration was probably wrong because
`vertical-align` is normally an inherited property. **That was wrong** - the fourth time
this session that a spec detail I asserted was corrected by someone who read the document.

CSS 2.1 **Appendix F, "Full property table"**, verbatim:

```
'vertical-align'
baseline | sub | super | top | text-top | middle | bottom | text-bottom | <percentage> | <length> | inherit
baseline                                  <- initial value
inline-level and 'table-cell' elements   <- applies to
no                                        <- Inherited
refer to the 'line-height' of the element itself
```

`Inherited: no`. The same table confirms the rest of the registration is right:
`table-layout` is `no`; `border-collapse`, `border-spacing`, `caption-side` and
`empty-cells` are all `yes`.

Two precisions worth keeping:

- **Appendix F is informative, not normative** - it says so itself. The normative statement
  is in the individual property definition, §10.8.3 for `vertical-align`. That page
  (`w3.org/TR/CSS21/visvert.html`) 404s from this machine while the index and `propidx.html`
  do not, so the informative table is what can actually be read. Cite the definition section
  in code; use Appendix F to check a flag quickly.
- **This is the internally consistent choice, not necessarily the browser choice.** The
  table work implements CSS 2.1 §17, and §17.5.3 distributes a table's own `vertical-align`
  to its row groups **explicitly** rather than by inheritance. So `Inherited: no` plus
  explicit distribution in the table algorithm is self-consistent, and it is what makes the
  row-group test correct. Browsers today inherit `vertical-align`; that divergence deserves
  a deliberate decision later rather than an accident.

### What actually needs fixing

The parser is right. `table.rs:287` tests **presence**, which is not **author intent**:

```rust
match style.and_then(|style| style.get("vertical-align")) {
    Some(_) => Self::table_vertical_align(style),      // "the group declared it"
    None => Self::table_vertical_align(table_style),   // §17.5.3: it declared nothing
}
```

The fix is to compare the *specified* value rather than the computed one, which needs the
missing API and is therefore a coordinated two-crate change:

1. **`render-css`** gives `ComputedStyle` a query that returns `None` when only the initial
   or default applies. The cascade already knows which declarations won, so the information
   exists; it is simply not exposed. A layout-side workaround in `table.rs` would hide the
   capability gap behind one call site's local fix, which is
2. **`render-layout`** uses it at `table.rs:287`.

The other five properties are unaffected: every one of their solver call sites compares
against the initial keyword, which registration reproduces exactly.

| **Four inline text properties had computed values and no consumer**: `text-overflow: ellipsis`, `text-indent`, `letter-spacing`, `text-transform` (plus `word-spacing`). | `crates/render-layout/src/solver/{inline,tree}.rs`, new `solver/text_tests.rs` | 29 new tests. `text-indent` supports lengths, percentages, `em`, negative values, and `hanging` / `each-line`. `letter-spacing` and `word-spacing` go through the **measurement** seam, not just paint, on both the normal and `pre` paths. `text-transform` is applied where the DOM text is read into the formatting tree, and a test reads the text back afterwards to prove the DOM is not mutated for any casing keyword. `text-overflow: ellipsis` correctly requires `overflow: hidden` and a non-wrapping line, and is not inherited. |
| **Six table properties were only in the token-level computed map**, so the layout solver hand-parsed strings for them. | `crates/render-css/src/{properties,computed}.rs` | `border-spacing`, `caption-side`, `vertical-align`, `border-collapse`, `table-layout` and `empty-cells` are typed values now, each grammar cited, each inheritance flag checked against CSS 2.1 Appendix F. |
| **CSS Nesting was unimplemented.** | `crates/render-css/src/{selector,stylesheet}.rs` | `&` desugars to `:is(parent)` at the token level, which gets matching *and* specificity right from existing machinery and avoids the cross-product blowup the spec warns about. The subtle part is CSS Syntax §5.5.5's declaration-vs-rule test: `div:hover {}` must become a nested rule while `margin: calc(50% - 10px) auto` must stay a declaration, and only a `{}` block disqualifies. **Measured corpus impact: 0 rules / 0 bytes** - the `&` characters in the corpus are all inside `url(...)` query strings. The tool was made nesting-aware first, then validated against a file that does nest, so the zero is a real absence rather than a blind spot. |
| **`color()` was unparseable**, and `display-p3` was dropping declarations. | `crates/render-css/src/properties.rs` | `srgb` and `srgb-linear` exact; `display-p3` by the documented CSS Color 4 conversion. Validated against the spec's own §2 example - `color(srgb 0.41587 0.50367 0.36664)` and `color(display-p3 0.43313 0.50108 0.3795)` both give `rgb(106, 128, 93)`. **Every other space is rejected with a diagnostic naming the space**, which required extending `PropertyParseError` with a `detail()` so a rejection says *why*. Typed rejections across the corpus: 11 → 6. |

### Two deliberate omissions in the text work, worth keeping in view

- `text-transform: full-width` and `full-size-kana` are not implemented. `full-size-kana`
  needs the Appendix G table and `full-width` needs UAX #11; both keywords are at risk in
  the corresponding CR. Implementing them partially would be worse than not offering them.
- §7.2's "ignore `Cf` characters" rule is not implemented - there is no UCD data in that
  crate, and a partial table would be wrong rather than merely incomplete.

## S23 - `text-decoration: none` is ignored on every page (highest user-visible impact)

Reported by direct observation, not found by inspection: pages that remove link underlines
still show them everywhere.

The chain, confirmed link by link in the code:

1. The UA stylesheet sets `a:link { text-decoration-line: underline }`. That is **correct** -
   browsers underline links by default.
2. A page writes `a { text-decoration: none }`. `expanded_declaration` in
   `crates/render-css/src/cascade.rs:524` handles `background`, `margin`/`padding`,
   `border*`, `font` and the legacy `grid-gap` longhands. **There is no `text-decoration`
   case.** So the author's `none` is stored under the literal key `"text-decoration"` and
   `text-decoration-line` is never overridden.
3. `crates/render-core/src/paint/display_list.rs:2380` reads `text-decoration-line` first and
   finds the UA sheet's `underline`. The `style.get("text-decoration")` fallback at `:2388`
   only fires when the longhand is **absent** - and the UA sheet guarantees it never is.

So `text-decoration: none` is dead code on every page on the web. Anchors, nav bars,
`a:hover` underline toggles, `abbr`, and `<a>` inside headings all keep their lines.

**This is an omission, not a spec question.** CSS 2.1 §16.3.1 lists `text-decoration` as the
shorthand for `text-decoration-line || text-decoration-color || text-decoration-style`, and
CSS Text Decoration 3 adds `-thickness`. The engine already expands four other shorthands;
this one was simply missed. **One case in `expanded_declaration` fixes it, and the paint
layer needs no change at all** - the longhands then take part in the cascade normally, the
author's `none` beats the UA sheet's `underline`, and the fallback stops firing.

### Closed: the shorthand, and a second claim of mine that was wrong

`expanded_declaration` gained a `text-decoration` arm. The fix mechanism is not tidiness - it
is that **Text Decoration 4 §2.6 says "omitted values are set to their initial values"**, so
all four longhands are always emitted. That is what lets an author's
`a { text-decoration: none }` beat the UA sheet's `text-decoration-line: underline`: the
shorthand has to write `text-decoration-line: none` explicitly to compete with it.

Measured over `.diag/**`: **123 `text-decoration` shorthand declarations, 123 now expand, 0
do not.** Of those, **62 resolve to `text-decoration-line: none`** - precisely the subset that
was dead against a user-agent underline. The rest: 58 `underline`, 2 `underline dotted`,
1 `line-through`. Unreadable values (`bogus`, `underline underline`,
`underline calc(2px)`) deliberately stay unexpanded and keep the existing paint fallback.

**The brief was wrong about the grammar, twice.** CSS 2.1 §16.3.1 is
`none | [ underline || overline || line-through || blink ]` - **line keywords only**. The
`<line-style> || <line-width> || <color>` form is **CSS 1**. The four-slot grammar including
thickness is **Text Decoration 4 §2.6**, not Level 3. All three are accepted, since the union
is a superset and real sheets mix them.

### Corrected: decoration propagation is already correct, and my "bug" was not one

I previously wrote that `display_list.rs:1288` was defective because an intermediate inline
setting `text-decoration-line: none` does not switch off an ancestor's decoration, and I
proposed moving propagation into the cascade and deleting the walk. **That was wrong.**

Per CSS 2.1 §16.3.1: *"The 'text-decoration' property on descendant elements cannot have any
effect on the decoration of the ancestor."* Text Decoration 4 restates it for style and colour
(§2.2, §2.3), and the current editor's draft contains no cancellation language at all.

So the existing walk is not an approximation of the rule - it **is** the rule: walk up taking
the first decoration that specifies a line *other than* `none`, and keep walking past `none`.
Implementing what I proposed would have un-underlined text the specification requires to stay
underlined. It is now pinned by
`a_descendants_ancestors_none_does_not_reach_the_ancestors_decoration` so it cannot be
re-litigated.

The spec also dissolves the apparent conflict between "not inherited" and "reaches
descendants" by separating two mechanisms: the longhands do not inherit, but the element
becomes a *decorating box* that applies to its own fragments and propagates downward (§2).
The longhands travel **as a set from the originating element**, and descendants cannot modify
them. So a registry comment saying propagation is "not the registry's business" is right.

What genuinely remains open on this row is the **downward** half: walking down from the
decorating box, which is a `render-core` paint-side change, not a cascade one. The walk
deletion I proposed should **not** happen.

## S24 - Closed, see "S24 - Closed: form owner is derived, not stored" below

Superseded. There **was** no form-owner concept in `render-dom`; there now is, derived on
read rather than stored. Kept as a pointer because the "what was missing" list below is
still the right description of what `render-js` and `render-browser` have to consume.

Found by grepping for what nobody had looked at. There is **no form-owner concept in
`render-dom` at all** - searching for `form` returns only `formatter` and `format` substrings.
And `FormData` exists in `render-js` as a standalone data structure with
`append`/`get`/`set`/`has`/`delete`/`entries`, connected to no form element.

So the most-used interactive path on the web - a search box and a login form - has no
mechanism underneath it. `HANDOFF.md` mentions a "formless submit fallback" added for the
baidu search box; that is a bypass, not spec behaviour, and extending a bypass is how the
bypass keeps being needed.

What is missing, per HTML §4.10.2 and §13.2.6.4.7:

- The **form owner pointer** on form-associated elements, and its reset rules. The subtle one:
  when a `form` ancestor is removed the owner is reset to **null** - the element does not keep
  pointing at the detached form and does not search further up.
  `<form><div><form><input name=a></form></div></form>` is the canonical shape.
- Re-association when the ancestor chain changes, and on wholesale replacement such as
  `innerHTML`, so the pointer cannot go stale.
- The form-associated element set: `button`, `fieldset`, `input`, `object`, `output`,
  `select`, `textarea`, `img` as associated-but-not-submission-capable, plus the
  `form-associated` custom-element hook. `form` itself is not form-associated, and
  `fieldset` has its own descendant-collection rules.
- The parser's **form element pointer**, which has to agree with the DOM's owner rather than
  being a second, divergent copy.

The DOM-side owner relationship is dispatched. `render-js` and `render-browser` need to
consume it - `form.elements`, `form.length`, `form.requestSubmit()`, `form.reset()`,
`formaction`/`formmethod`/`formenctype`/`formtarget`, and `FormData` built from a form - and
none of that can start until the owner exists.

## S17 - Closed: two media-query evaluators, and the cost was higher than a warning

There were two independent media-query evaluators and they disagreed:

- **The real one**, `media_query_list_matches` in
  `crates/render-css/src/cascade.rs`, used by the cascade to decide which rules apply. It
  evaluates `width`, `min-width`, `max-width`, `min-height`, `max-height` against
  `MatchContext.viewport_width/height`, and it handles the real-world
  no-spaces-around-`and` form.
- **The weak one**, in `crates/render-core/src/document.rs`, understood only the media
  *type*. `media_type.filter(|_| words.next().is_none())` marked any query containing a
  feature as unsupported, because the next word after `screen` is `and`.

**I recorded this as a cosmetic false warning. It was not.** The flag fed
`is_eligible()`, so a false "unsupported" made `media_matches` false, which meant:

> **`<link media="screen and (min-width:768px)">` was dropped from the plan entirely and
> never fetched.**

So responsive stylesheets - the single most common CSS feature on the modern web - were not
merely mis-warned about, they **were not being loaded at all**, and the warning was the only
trace. That is the second time in this project that a diagnostic misdescribed reality and the
real cost was much larger than the diagnostic (the first was `srcset` being reported
unsupported when it was implemented).

Both answers now come from the cascade: `media_query_list_matches` for the match and
`media_query_list_is_supported` for the flag, so the two cannot drift. `render_core` hoists
one `MatchContext` with the real viewport and feeds both the cascade and resource discovery.
`render-browser`'s fetch planner should adopt `discover_author_style_slots_with_context` - the
signature-preserving `discover_author_style_slots` is still there, but a viewport-dependent
`media` attribute will read as undecidable there.

**Three pre-existing tests encoded the defect** and had to change: each asserted
`MediaQueryUnsupported` for `screen and (min-width: 1px)`. They were substituted with
`screen and (hover: hover)` - a feature the engine genuinely cannot evaluate - so each still
exercises the same path, with positive cases added alongside. This is the one change in the
round that wants independent review, because editing a test to accommodate a behaviour change
is exactly where a real regression hides.

## S5 - Closed: inline SVG parses *and* rasterises

Both halves are done. The parsing half was recorded earlier; the rendering half used the
existing `crates/render-core/src/image/svg.rs` with no new rendering code, via a new
`src/image/inline_svg.rs`: discover `svg` elements in the `Svg` namespace, serialise the
subtree with the namespace-aware `serialize_html_node`, rasterise, and register the result as
an image resource.

What now renders: `<rect>`, `<circle>`, `<ellipse>`, `<polygon>`, `<polyline>`, `<line>`,
`path` (`M m L l H h V v C c S s Q q T t A Z z`), `<g>`/`a`/`switch` transforms,
`fill`/`stroke` inheritance, and `viewBox` scaling.

**One seam did not exist as planned, and the report says so rather than working around it
silently:** `render-layout`'s `replaced_size` hard-gates on `img | video`, so a registered
image on an `svg` node is *not* consumed for sizing. Sizing therefore comes from the
`width`/`height`/`viewBox` attributes - which is what the brief specified anyway - fed into a
new `svg::decode_svg_viewport(bytes, w, h, limits)`.

Deliberately left out, each documented in the module: `use`/`symbol`/`defs` indirection
(needs a shadow tree - an honest absence, not a stub), `foreignObject` (its HTML lays out as
ordinary in-flow HTML, correct only for `x=0 y=0` with no scaling), aspect-ratio preservation
when the box ratio differs from the viewBox, and a percentage `width` resolving to the
viewBox extent rather than 100% of the container.

One UA rule was **chosen rather than quoted**, and is commented as such: `svg { display:
inline-block }`, because `inline` is a character-level box with no geometry. By contrast
`svg { overflow: hidden }` is SVG 2 §3.11 verbatim, and the never-rendered list is SVG 2
§3.2.1 verbatim, declared as data and asserted against the spec text so the sheet rule and the
constant cannot drift.

## S7 - Quirks mode: parse side done, behaviour not started

Correcting the earlier entry: **`MatchContext.quirks_mode` is not inert.** `selector.rs:1329`
and `:1337` read it for quirks-mode case-insensitive `id` and `class` matching, which is
spec behaviour.

- **Parse side: done.** `doctype_quirks_mode` computes all three of
  `NoQuirks` / `LimitedQuirks` / `Quirks` from the public and system identifiers,
  `force_quirks`, and the missing-doctype path.
- **The wiring gap is one line.** `render-core` builds the `MatchContext` with
  `..MatchContext::default()` and never sets the flag. **`render-browser`'s
  `render_worker.rs` already sets it**, so the browser side is wired and only the core side
  is not.
- **The behaviour is a long way off.** Everything CSS 2.1 §9.2.1.1 specifies - the UA-sheet
  deltas and the box-model rules - is unimplemented. §9.2.1.1 has **not** been read yet; the
  page that was fetched turned out to be chapter 10 rather than chapter 9, so the item list is
  deliberately not being written from memory. Whoever picks this up reads §9.2.1.1 first.

## S25 - `MissingWhitespaceBetweenAttributes` fires for the wrong attribute

Found by the acceptance harness, not by inspection.
`crates/render-html/src/tokenizer.rs:477-483` reports the diagnostic for the attribute
**after** a valueless one, because `had_whitespace` is not carried across `consume_attribute`
when the attribute has no value.

| Markup | Reported |
| --- | --- |
| `<script defer src="a.js">` | `MissingWhitespaceBetweenAttributes` |
| `<script src="a.js" defer>` | clean |
| `<div a b>` | `MissingWhitespaceBetweenAttributes` |
| `<div a="1" b="2">` | clean |

`<script defer src=...>` is near-universal on the real web, so this is not a corner case: it
means the single most common script tag on the web reports a parse error. It is a diagnostic
only - the parse itself is correct - which means the cost is a flood of false parse errors
that will drown real ones, and any tooling that counts or gates on parse errors is misled.

It was found by `tests/real_site_tasks/`, which is the point of that harness existing: a
harness that asserts "no fixture reports any parse error" turns a miscounted diagnostic into
a red test instead of a log line nobody reads.

## Also noted by the harness: two more never-constructed diagnostics

`ImageDiscoveryDiagnosticCode::MissingSource` is declared and never constructed, exactly like
`SrcsetUnsupported` was before `srcset` was implemented. Both are dead variants. The harness
**permits rather than requires** `MissingSource`, so implementing it will not break the gate
- deliberately, so that closing a gap does not require touching the test.

## S24 - Closed: form owner is derived, not stored

Was: **no form-owner concept in `render-dom` at all**, and `FormData` in `render-js` was a
standalone data structure connected to no form. The most-used interactive path on the web had
no mechanism underneath it, and the `formless submit fallback` in `HANDOFF.md` was a bypass
rather than spec behaviour.

### The decision that matters

**`Dom::form_owner(node)` derives the owner on every read.** There is no owner field to keep
in step with anything, so the entire class of staleness bugs - reparenting, `innerHTML`,
wholesale replacement - cannot exist.

The three steps are the spec's (§4.10.18.3), each a pure function of the tree and attributes
as they are right now:

1. The parser's association, if it still holds.
2. Else, if the element is **listed**, has a `form` content attribute, **and is connected**:
   the first element in the element's own tree, in tree order, with that ID, and only if it
   is a `form`.
3. Else the nearest ancestor `form`, walking `Node::parent()` and stopping at the first one.

**Exactly one datum is stored, because exactly one thing needs storing**: the HTML parser can
associate a control with a form that is not its ancestor - that is what the form element
pointer is *for* - and the finished tree does not say so. It is a field on the node,
`ElementData::parser_inserted_form_owner`, holding the form **and the parent the element was
created for**. It is invalidated **by being read**, not by a hook: it applies only while
`self.parent(node) == Some(link.intended_parent)`, so any move or reparent drops it and the
derived rules take over. No mutation path in the DOM has to remember to update it.

Two consequences worth knowing, both measured and pinned:

- The owner is derived, so a **detached** control still has one. That is correct - "removed
  from a document" is a step *inside* the spec's reset algorithm, after which the ancestor rule
  re-associates, so a control still inside a detached form keeps that form.
- Step 2's "is connected" clause means a control in a `<template>`'s contents **ignores** its
  `form=` attribute. Clone-then-query, not query-then-clone.

### The reset rule, which is the part that is easy to get wrong

When a `form` ancestor is removed, step 3 cannot reach it, so the owner is `None`. It cannot
keep pointing at the detached form because **nothing stores a pointer to point at**, and it
cannot search further up because the walk stops at the *first* form, not the outermost.
`removing_a_form_ancestor_resets_the_owner_to_null` asserts all three halves in order,
including that the still-connected outer form is **not** substituted.

### A rule I described that no longer exists

I said `fieldset` "has its own descendant-collection rules" and implied more than that. **The
rule I was expecting has been removed from the standard.** "Listed elements" is now a flat
list - `button`, `fieldset`, `input`, `object`, `output`, `select`, `textarea`, plus
form-associated custom elements - and the old "*not a descendant of a fieldset element whose
descendants are not listed elements*" clause went with the old definition. The standard's
eight pages were grepped for the old wording: zero hits.

So: `form.elements` is listed elements whose owner is this form, **minus image-button
inputs**; a `fieldset` is itself listed, so it appears in its own form's list; `img` is
form-associated but not listed, so it is not. And `fieldset.elements` is a **descendant
filter, not an owner filter** - the root scopes it, the filter is only listed-ness, so a
listed descendant owned by a *different* form is still in it. `disabled` changes neither list;
it affects constraint validation and submission, which are not modelled.

### Consumption contract, already in the DOM

Nothing needs to be added to `render-dom` for the consumers:

- `Dom::form_owner(element) -> Option<NodeId>`, plus `is_form_associated`,
  `is_listed_element`, `is_submittable_element`, `is_form_element` - all namespace-aware.
- `form_owner_elements(form)` and `listed_elements_within(root)`, both tree order, both `&self`
  and allocating. **Call them per access; do not cache across a mutation.**
- The critical warning for the browser side: **the form is not necessarily an ancestor.**
  A control can be owned by a form reached through the parser's form element pointer. Do not
  find the form by walking the tree; `form_owner` is the only correct answer.
- `None` means "no form", and a `form=` attribute naming nothing leaves the owner `None`
  **even when the control is inside a form** - that is how an author discovers the typo, so
  do not substitute the nearest ancestor.
- `label.form` is the *labeled control's* owner, not the nearest ancestor form.

`FormData` can now be built from `form_owner_elements(form)` filtered to submittable controls,
excluding disabled ones and those inside a disabled `fieldset`. The entry-list construction
algorithm and the `past names map` are **not** implemented, so a control that changes its
`name` is not remembered.

The `formless submit fallback` can now be retired in favour of this.

## S25 - Closed: `MissingWhitespaceBetweenAttributes` fired for the wrong attribute

Found by the acceptance harness, not by inspection. The tokenizer reported the diagnostic for
the attribute after a *value-less* one, because `consume_attribute` ended by skipping the
trailing whitespace **and discarding whether it had**, so the caller's `had_whitespace` could
only ever read false. `<script defer src=...>`, which is on almost every page, reported a parse
error; the parse itself was always correct.

The standard raises this error in **exactly one state**, 13.2.5.39 "after attribute value
(quoted)". So the signal is *which state the previous attribute left the tokenizer in*, not
*is whitespace lying around in the stream*. `consume_attribute` now returns what **ended** the
attribute - a quoted value, or nothing capable of producing a missing separator - instead of
letting the caller infer it.

`<div a b>`, `<div a b="1">`, `<div a="1" b>` and `<div a="1" b="2">` are all clean, and the
reachable class, `<div id="foo"class="bar">`, still reports once per gap at the offset of the
gap. Five tests in `crates/render-html/src/tokenizer.rs` pin the distinction, each also
asserting the attribute list so the diagnostic cannot be traded for a parse change.

**A note on the criterion this was given, because it was wrong.** The work order asked for the
diagnostic to fire on `<div a b>`. Per 13.2.5.34 the "after attribute name" state reports
missing-whitespace-between-attributes for *nothing at all* - whitespace is ignored there, `/`
and `>` end the tag, `=` goes to the value state - so no attribute can follow a value-less
attribute with a missing separator.

**The class that criterion wanted preserved does not exist.** Two value-less attributes can
never be adjacent without whitespace, because every character that can end a name
(`\t\n\f\r space`, `/`, `>`, `=`) is either a separator or ends the tag, and anything else is a
name character. `<div ab>` is **one** attribute named `ab`, not two. So `<div a b>` is clean
because that is what the standard says - not because the cases were collapsed. The
implementer followed the standard over the criterion and said so, which is the correct
resolution when the two disagree.

## S13 - Interaction and document-level features

- **No Selection or Range API.** `getSelection`, `createRange`,
  `caretRangeFromPoint` and `caretPositionFromPoint` appear nowhere. The
  `ContentSelection` type in `crates/render-browser/src/app.rs:3210` is the shell's own
  hit-test result for hover and click, not the DOM Selection model. Consequence: a user
  cannot select text, cannot copy, and page scripts that call `getSelection()` throw.
- **No scroll containers - scrolling is page-level only.** `FragmentTree` exposes a
  single document-wide `scrollable_content_size` with `max_scroll_offset` and
  `clamp_scroll_offset` (`crates/render-layout/src/fragment.rs:104-141`). Nothing
  represents a scrollable box. So `overflow: auto` or `overflow: scroll` on a `div`
  does not create a scrollport: a fixed-height feed, carousel track, sidebar, or
  virtualised list overflows its box and is simply clipped, with no way to reach the
  hidden content. This is a common real-page shape, and it is invisible in a full-page
  screenshot because the content is not missing from the document, only from the box.
- **No paged output.** There is no print rendering and no `@media print` evaluation.
  `crates/render-core/tests/wpt_reftests.rs:432` lists `print` only as a media type to
  skip.
- Not implemented, and not yet audited for real-page impact: `scroll-snap`,
  `content-visibility`, `subgrid`, `@container`, `::marker`, `forced-colors`.

Note that media *features* are not wholly absent: `@media (prefers-reduced-motion)` and
`@media (prefers-color-scheme)` are used by the built-in new-tab page at
`crates/render-browser/src/home.rs:244,250`, so at least those evaluate.

---

## Correctness traps found while auditing (verify, do not trust old reports)

- `docs/qq-compatibility-analysis.md` claimed `:hover`/`:focus` always match. That
  report describes the retired Python/Qt architecture and must not be used as evidence
  about the current engine. Re-verify against the code.
- `docs/qq-compatibility-analysis.md` also claimed `document.cookie` was missing. It is
  implemented at `crates/render-js/src/runtime/eval.rs:2817` and `:3309`. The report
  predates the fix.
- `srcset` **is** implemented (`render-core/src/image.rs:952-1152`, including `sizes`
  and both `w`/`x` descriptors). `docs/real_site_acceptance.md` still describes
  `SrcsetUnsupported` as an expected warning; that text is stale.

## Ordering used for scheduling

S2 and S3 were scheduled first because they were pure CSS with a small blast radius and
they made every site look like a page instead of a text dump. Both are now closed. The
remaining order is:

**S1**, then **S5's rendering half**, then **S12**, then S4, then S6, then S7, then S16,
then S10.

S1 has the largest visual payoff but spans `render-layout` and `render-browser`, so it is
sequenced after the agents that own those crates finish, and it must be done as one
change - see the note in `HANDOFF.md` about not splitting it.

S10 is deliberately not last. A missing global throws a `ReferenceError` at the point of
use, which kills the surrounding script - so `localStorage` being absent can blank a page
that would otherwise render fine, no matter how correct the layout engine is. The visual
fixes cannot be observed on such a page at all, which makes the whole visual programme
hard to evaluate.

## A note on citing line numbers

Citations in this file are `path:line`. That is precise, and it goes stale the moment the
file is edited - which, while several agents are working, is often. For a symbol in a
file under active edit, cite the **symbol** rather than a bare line number. Two claims in
this file had already drifted by the time they were checked: the `document.cookie` line
numbers moved twice in one session. If a citation here disagrees with the code, the code
wins and the citation should be corrected rather than the finding.

## A working rule that this session earned the hard way

**My spec knowledge is a worse source than the live spec document, and an agent that
fetches the current text will find errors in what I hand it.**

Three times in one session an agent returned a work order and said, in effect, "the
brief is wrong about the standard":

1. `xml:base` is not in the HTML standard's 11-entry "adjust foreign attributes" table.
2. `xmlns:xlink` maps to the **XMLNS** namespace with prefix `xmlns` and local name
   `xlink` - not to the XLink namespace, which is what I wrote.
3. `in select` and `in select in table` **are not insertion modes in the current
   standard**. The standard defines 21 modes at 13.2.6.4.1-.21 and the select rules live
   in "in body". Implementing what I asked for would have introduced a violation.

All three times the agent was right, and in the third case refusing was the whole value of
the round: a plausible, confident, wrong brief would have produced confidently wrong code
with tests asserting the defect.

The rules that follow, for briefs and for me:

- **Never state a spec detail from memory in a work order.** Give the section number and
  let the implementer read the text. "Per CSS 2.1 §17.5.3" is safe; "the HTML rendering
  section sets `table > tr { vertical-align: middle }`" was not, and it was false.
- **An agent that contradicts the brief must be believed until the standard says
  otherwise**, and the resolution has to be checked against the document, not against
  either party's confidence.
- **A spec citation in code must be verified, and an unverifiable one deleted rather than
  kept.** The `vertical-align` comment in this project claimed a UA stylesheet rule that
  no browser has; it survived review because nobody opened the standard.
- **A test must never encode a defect as expected behaviour.** Several agents caught
  themselves doing this and corrected the test rather than the code; that is the signal
  the standard is working.
