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

## S4 - Partly closed: the silent discard is now reported; evaluation is not implemented

`crates/render-css/src/stylesheet.rs` handled `ParsedRule::KeyframesBlock` and
`ParsedRule::FontFaceBlock` by dropping them **with no diagnostic at all** - they returned only
a `location` and `flatten_rules` did `let _ = location;`, while a genuinely unknown at-rule
correctly produced "is parsed but not evaluated yet". Grepping `crates/render-core` for
`@font-face`, `keyframes`, `animation` or `transition` returns nothing.

Counted on the real corpus by `css_corpus`: **223 `@font-face` blocks and 249 `@keyframes`
blocks** parsed and thrown away.

### Closed half: they are now reported, not silent

The split is a **table, not a control-flow accident**: `crates/render-css/src/at_rules.rs` holds
`RECOGNISED_AT_RULES: &[(&str, &str)]` - 22 at-rules each carrying the section that defines it -
and `at_rule_support(name) -> {Evaluated, Unimplemented, Unrecognised}`. The citation is what
makes "recognised" a claim rather than an assertion, and a test asserts nothing outside the table
answers with one.

| | before | after |
| --- | --- | --- |
| `@font-face` | 223, **0 diagnostics** | 223, **223 diagnostics** |
| `@keyframes` | 249, **0 diagnostics** | 249, **249 diagnostics** |

The 249 break down as 120 `@keyframes`, 81 `@-webkit-`, 18 `@-moz-`, 15 `@-o-`, 15 `@-ms-`. The
prefixed spellings resolve to the at-rule they alias, and the diagnostic quotes **the spelling
the author wrote**, which is the only useful thing to show someone looking at their own CSS.

`@charset` is deliberately **out** of the table, with the reason recorded: CSS Syntax §4.1 has
cssparser consume it before the at-rule parser runs, and Conditional 5 §2.1.2 says it is not a
valid at-rule. A test pins that `@charset` produces no diagnostic, so nobody adds it later and
then expects one.

**Three partial drops of the same bug, one level down, all fixed:**

- `@font-face`'s descriptor list was parsed by `parse_declarations` and **both results thrown
  away**. A test now asserts `@font-face { font-family 'X' }` yields exactly the descriptor's own
  error *and* the not-evaluated reason - the inner diagnostic must not be swallowed by the outer
  report.
- A `@layer` block nested inside a style rule reported "@layer is not evaluated yet", which is
  false - top-level `@layer` **is** evaluated. A new `NestedLayerBlock` variant carries the
  accurate reason.
- `PropertyParser::rule_without_block` returned `Err(())` for every nested statement at-rule,
  which is a silent drop. A nested `@layer a, b;` now registers layer order (Cascade 5 §7.1);
  everything else reports.

The diagnostic also **survives conditional grouping**, which needed no new machinery because
`flatten_rules` already recursed - the only change was emitting at the point of the discard
instead of nowhere. Seven shapes are tested: inside `@media`, inside `@supports`, both orders,
inside `@layer`, `@layer` inside `@media`, and nested in a style rule's block contents.

### Open half: evaluation

No webfont on any site - nearly every large Chinese site self-hosts a CJK face - and no CSS
animation or transition anywhere.

Correctly **not** done as a half-feature, and the reasoning is now load-bearing: a parse-only
`StyleSheet::font_faces` that nothing can consume would satisfy the parser and change no
rendering. Worse, the diagnostics prove it - the moment a font backend that selects by family
and weight lands, "parsed but not evaluated yet" becomes visible on 223 blocks per stylesheet,
so a parse-only half-feature would be a lie the diagnostics themselves would expose.

The real work needs `render-net` (font file fetching), face registration and glyph
rasterisation, and an animation clock. `@font-face` evaluation is now correctly sequenced behind
S1, because the backend it needs to register faces with is the one S1 makes select by
`(family, weight, style)`.

## S4b - Closed: `@supports` is evaluated, and it was lying in a way that looked fine

Was: all **93** `@supports` blocks applied **unconditionally**. The condition was parsed and
then ignored, with a comment acknowledging it only "honors the common `@supports (display:
grid)` blocks".

**My own assessment of the impact was wrong, and it is worth recording why.** I had written
that "on this corpus the effect is benign (all three distinct queries are positive, one with
`or`)". The measurement says **3 true, 90 false**. The three distinct queries I had seen were
the three that survived some earlier filter, not the shape of the corpus - so the harm looked
absent precisely because the sample was unrepresentative.

| | before | after |
| --- | --- | --- |
| `@supports` blocks | 93, all applied | 93 evaluated: **3 true, 90 false**, 0 invalid, 0 undecidable |
| rules inside `@supports` | **98 applied** | **6 applied, 92 dropped** |
| declarations reaching the cascade | 42356 | 42205 (**151 stopped**) |

**91 of the 92 dropped rules are one site.** They are
`(color: lab(from red l 1 1%/calc(alpha + 0.1)))`, which this engine rejects with a typed
diagnostic. So that site was getting its `lab()` enhancement **and** its fallback applied
simultaneously and the author got neither - the specific failure `@supports` exists to prevent.
The corpus report's `inert` column now moves in step with the engine, because its tokenizer
walk asks the same `evaluate_supports_condition` and therefore cannot contradict it.

### Why the oracle lives in `render-css` and needs no `render-core` input

Conditional 3 §6.1 defines support as *"accepts that declaration rather than discarding it as a
parse error"*, and every function that decides that already lives in this crate:
`properties::parse_typed_property` (via a new `DECLARED_GRAMMARS` const, so the support set is
declared rather than buried in a `match`), `computed::PropertyRegistry::standard_baseline()`,
and `cascade::expand_shorthand` - **the same three the cascade reads**. One evaluator, one
answer.

That is the specific lesson of S17: a second, weaker parse of the same question is how this
project shipped a false `MediaQueryUnsupported` on **every** `@media (min-width: ...)`.
Shorthands answer as the conjunction over their longhands, per §6.1's "implement all parts".

### Three states, not two

`font-tech()`, `font-format()` and `at-rule()` return
`SupportsCondition::Undecidable(Vec<UnansweredFeature>)` - **never a bool**. The reasons name
the crates, e.g. `at-rule(@font-face)` reports that its `src` must be fetched by `render-net`
and its face registered with a font backend that selects by family and weight in
`render-browser`. The block is dropped **and** diagnosed.

`selector(...)` is **answered, not guessed**: `parse_selector_list(arg).is_ok()` and exactly one
complex selector. So `selector(:has(> img))` is `true` here while `selector(col || td)` - the
spec's own example - is `false`, because this engine's selector parser has no column
combinator. That is a real answer about this engine, which is what the condition asks.

One deliberate deviation, commented in the code: on `at-rule()` it does not follow §2.1.2's
literal "would accept an at-rule beginning with the specified at-keyword" wording, because that
test is syntactic and this crate **does** accept `@font-face` - so reporting `true` would claim
webfonts work on pages whose 223 faces are all dropped.

### Still open on this row - both are one-liners, and neither is scheduled

- **`CSS.supports`** in `render-js` should call
  `render_css::supports::supports_condition_text`, which already implements §7.5 including the
  wrap-in-parentheses retry.
- **The diagnostics panel.** `render-core/src/document.rs` already forwards every
  `sheet.diagnostics` entry into `DocumentRenderDiagnostics::style_sheets`, so the new
  diagnostics are in the pipeline output with **no `render-core` change**. But
  `render-browser/src/render_worker.rs:212` only *counts* them under `RENDER_DEBUG_FRAME`, and
  each message still needs mapping to a `StylesheetDiagnosticCode` variant - `resources.rs:73`
  has `CssSyntax`, and the at-rule and undecidable-`@supports` messages need a sibling code or
  the panel shows nothing at all.

So the diagnostics this round produced are **counted but not displayed**. That is a real gap,
and it is the difference between "the engine told us" and "we can see that the engine told us".

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

## S20 - Closed: the same-origin ceiling is now a measured decision

Was: `max_idle_connections_per_host` defaulted to **3**, so a page pulling 40 assets from one
origin over HTTP/1.1 had three idle slots to reuse and serialised the rest behind fresh
handshakes. A capacity limit rather than a correctness bug, which is why it was easy to miss -
but the ceiling was **decided by a dependency's default rather than by anyone here**.

Now two explicit `FetchConfig` fields: **6 per origin, 24 total**. Six is not picked for its
size - it is the per-origin concurrency `BatchOptions::default()` was *already* applying via
`FixedOriginLimit(6)`. Keeping fewer idle connections than may be in flight throws away sockets
the next request would have taken for free; keeping more holds sockets that could never be
reused. A test asserts they agree **through the policy**
(`origin_policy.max_concurrency(...) == idle_connections_per_origin`), not merely through the
shared constant, so they cannot drift.

Measured on a local origin serving 40 distinct keep-alive paths, 8 runs each:

| idle/origin, total | connections for 40 assets |
| --- | --- |
| 3, 10 (the old defaults) | 10, 9, 9, 10, 8, 9, 9, 11 - mean **9.4** |
| 6, 24 | 6, 6, 6, 6, 6, 6, 6, 6 - mean **6.0** |

**The default was binding**, plainly: a 36% cut in handshakes, landing exactly on the floor for
six requests in flight over HTTP/1.1. Sweeping the per-origin ceiling from 1 to 12 gives
25, 20, 11, 7, 6, 6, 6, ... - flat from 5, so 6 sits on the knee rather than past it.

**Wall clock honestly measures nothing on loopback** and no number is claimed for it: the same
configuration ranged 134ms-729ms across those runs because other work was compiling on the box,
and a loopback TCP connect costs about 0.1ms. The value of a handshake is a function of RTT, and
the earlier real-CDN figure of roughly 85ms per avoided handshake is an **extrapolation from a
different measurement**, not something this test establishes.

`max_idle_age` is **deliberately not exposed**, because ureq 3.3's `Connection::age()` is
`now.duration_since(now)` and therefore always zero, so the setting can never do anything. A
setting that provably does nothing is worse than no setting. It is also not needed for
correctness, and that is now evidenced rather than asserted: an origin closing an idle socket
costs one wasted pool slot and one new handshake, **not a failed request**, because the agent
probes liveness before handing out a pooled connection.

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

## S6 - **NOT closed.** The geometry is correct and nothing applies it

> **Correction, and the most important one in this register.** I closed this row on the strength of
> 21 unit tests. The acceptance harness then measured the pixels and found that
> **`sticky_constraint`, `sticky_offset` and `sticky_fragments` have no consumer outside
> `render-layout`'s own source and its own tests** - nothing in `render-core`'s paint, page or
> interaction path, and nothing in `render-browser`. Verified independently: a page with a sticky
> bar rasterised at viewport origin `y=500` **finds white where red should be.**
>
> **So the layout half is right and the paint half does not exist.** This is precisely the failure
> the project's own rule forbids - "implement features fully or not at all" - and I recorded a
> closure for it while enforcing that rule on other agents. **A unit test on a pure function proves
> the function is correct; it says nothing about whether anything calls it.**

### What is actually there

- `crates/render-layout/src/solver/mod.rs:661` records `scrollport: self.options.viewport` for
  every sticky box, and `FragmentTree` stores the constraint.
- The **architecture is right and worth keeping**: layout never sees the scroll offset, the
  fragment tree is built once in document space, and the consumer adds `sticky_offset(...)` to the
  fragment and translates the subtree. That is why the fragment is never moved, so layout position,
  paint order and stacking are untouched - which is what the specification requires.
- **There is no such consumer.** The paint side was never written.

`StickyInsets` holds `Option<f32>` per side because **an `auto` inset is not a zero inset**, and
`sticky_offset` is a pure function with 12 tests asserting real geometry. All of that is correct and
all of it is unreachable.

### The paint-side contract, written down so the next implementer does not re-derive it

- Resolve `sticky_offset(constraint, scroll_offset)` per box and add it to the fragment's position.
- **Translate the subtree**, not the box, so descendants move with it.
- Clip to the scrollport, and use `max_scroll_offset` / `clamp_scroll_offset` for the range.
- **Input routes to the innermost scrollport whose `clip` contains the point** whose
  `max_scroll_offset` is non-zero, then bubbles outwards to the page.
- Not in `render-layout`, and not to be faked there: the offset store, scrollbar chrome, keyboard
  scrolling, scroll anchoring.
- **A sticky box inside a *nested* scrollport cannot be resolved** by `sticky_offset` as written,
  because the nearest scrollport's offset is unknown at layout time. That is a recorded dependency,
  not a papering-over.

### The two general defects found while building it, both fixed at the helper

1. **Under-constrained boxes got a phantom right margin.** CSS 2.1 §10.3.7 only adjusts a margin
   when the values are *over*-constrained, but the fall-through arm ran for the under-constrained
   case too, so `BoxGeometry::margin_rect()` stretched a 300px box to its containing block's 800px.
   That is a wrong used value on a **public geometry helper**, and `scrollable_content_size` reads it
   too.
2. **A table cell's `vertical-align` ignored a cell with a height of its own** - the offset came
   from the border box, which for a cell with a specified height equals its outer height, so
   `<td style="height:200px; vertical-align:bottom">` left its content at the top. `BlockResult` now
   carries `flow_height`.

### And the same bug is still live somewhere else (see the harness findings)

A nested scrollport's overflowing content **inflates the page's scrollable width**:
`scrollable_content_size` measures 1320 against a 1280 viewport, because a 1300px strip inside a
1240px scroller extends the document. **A real navbar that is its own scroller produces a spurious
horizontal page scrollbar.** That is defect 1 above, in a place its fix did not reach.

## S7 - Quirks mode, see "S7 - Quirks mode: parse side done, behaviour not started" below

Was: `render-css/src/properties.rs:758` defines the `Sticky` keyword and **no consumer
existed anywhere**, so a pinned header scrolled away with the content.

### The architectural decision, which is the point

**Layout never sees the scroll offset.** The fragment tree is built once in document space and
`render-core` translates it by `-viewport_origin` at raster time. So a scroll-dependent
displacement *cannot* be baked into a fragment without re-laying out the whole document on
every scroll tick.

- `crates/render-layout/src/sticky.rs` - `StickyConstraint { margin_rect, containing_block,
  insets, scrollport }` plus the pure `sticky_view_rect` and
  `sticky_offset(constraint, scroll_offset)`.
- `solver/block.rs` - one branch: a box whose computed `position` is `Sticky` is registered.
  **The fragment is never moved**, so layout position, paint order and stacking are untouched,
  which is what the spec requires - sticky creates no stacking context and does not affect the
  element's own layout position.
- `solver/mod.rs` - constraints resolve **after** layout, not during. This is not a stylistic
  choice: a relatively positioned ancestor, an out-of-flow ancestor, a table row, or a cell's
  `vertical-align` offset all move a subtree *after* the box inside it is finished, so a
  constraint recorded earlier would be stale.

`StickyInsets` holds `Option<f32>` per side because **an `auto` inset is not a zero inset**.

The clamp is per axis to `[block.start - box.start, block.end - box.end]`, plus one rule the
geometry alone does not give you: a box is displaced only while its containing block is at least
partly inside the sticky view rectangle, so a sticky element in a parent that has scrolled past
is not dragged back into view. At both ends of that range the box is off-screen, so the
transition is never visible.

12 tests in `solver/sticky_tests.rs`, each asserting concrete geometry, including that a
vertical scrollbar does not shift a `top`-constrained box horizontally and that a `bottom`-
constrained element further down the page than the scrollport reaches is not pulled in.

### Scrollports are now represented

`FragmentTree` exposes, per box whose `overflow` is not `visible`,
`ScrollportGeometry { mode, clip, scrollable }` with `max_scroll_offset()` and
`clamp_scroll_offset()`. `clip` is the padding box in document space; `scrollable` is the content
extent reachable past it.

**`overflow: hidden` and `overflow: auto` are structurally distinct, not a flag.**
`overflow_clip_mode` returns `None` for `visible`, `Clip` when no axis scrolls, and `Scrollport`
when either axis does. `a_clip_is_not_a_scrollport` asserts the two modes produce the **same
`clip` and the same `scrollable`** and differ only in **range** - so conflating them is not
possible to do by accident.

### Still open on this row

`z-index` (read as raw text at `render-layout/src/solver/mod.rs:399`; flex and grid children are
not sorted and the paint layer has no z-index awareness), multi-column layout, and the shell-side
half of scrolling: the offset store, scrollbar chrome, keyboard scrolling, scroll anchoring. A
sticky box inside a *nested* scrollport also cannot yet be resolved, because the nearest
scrollport's offset is unknown at layout time - recorded as a dependency, not papered over.

### Two general defects found in the helpers, both fixed at the source

1. **Under-constrained boxes got a phantom right margin.** CSS 2.1 §10.3.7 only solves the
   margin equation by adjusting a margin when the values are *over*-constrained, but the
   fall-through arm ran for the under-constrained case too, so `BoxGeometry::margin_rect()`
   stretched a 300px box to its containing block's 800px. That is a wrong used value on a
   **public geometry helper**, and `scrollable_content_size` reads it too - so a 300px element
   inflated the document's scrollable width and produced a spurious scrollbar.
2. **A table cell's `vertical-align` ignored a cell with a height of its own.** The offset was
   computed from the cell's *border box*, which for a cell with a specified height equals its
   outer height, so `<td style="height:200px; vertical-align:bottom">` left its content at the
   top. `BlockResult` now carries `flow_height` - what the in-flow children occupy, before a used
   `height` stretches the box.

A Hacker-News-shaped fixture is byte-identical before and after both fixes, so neither disturbed
the common case.

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

## S9 - Closed: the capability registry now tracks the gaps, and cannot decay back

Was: `crates/render-core/src/spec/registry.rs` defined 20 `FeatureDefinition` entries, **all**
`Partial`, with no entry at all for animations/transitions, `@font-face`, pseudo-elements, table
layout, inline SVG, video decode, or forms. A gap that is not registered cannot be tracked, and
cannot be reported as progress either.

**My "all 20 are `Partial`" was off by one** - `fetch.runtime` was the only non-`Partial` entry,
and it *under*claimed, since `fetch` and `XMLHttpRequest` are both implemented. It is now
`Partial` with CORS, streaming bodies and the cache named as what is not.

Now **36 entries**, adding `html.foreign-content`, `html.template-contents`,
`html.scripting-mode`, `html.forms`, `html.quirks-mode`, `css.pseudo-elements`, `css.nesting`,
`css.colors`, `css.media-queries`, `css.font-face`, `css.animations`, `css.transitions`,
`css.text-decoration`, `css.tables`, `svg.inline-rasterization`, `media.video-decode`.

**The mechanism that stops it decaying is the part worth keeping.** `FeatureDefinition` gained a
`notes: &'static str` field, and `validate_metadata` now **rejects any non-`Conformant` entry
with a blank note** (`FeatureRegistryError::MissingNotes`). A registry whose entries carry no
information is worse than no registry, because it looks like an answer. `FeatureDefinition` is
constructed only inside `src/spec/`, so no other crate breaks.

**Nothing is `Conformant`, and that is now a recorded decision in the file rather than an
accident**: no WPT area is imported, so the bar cannot be met. `Missing` where the engine has
nothing, `Partial` where something works.

`@font-face` is registered as **`Missing`**, deliberately. The acceptance harness's test asserts
the registry *declares* such a feature, not that it supports one, so registering it as `Missing`
makes the test pass **with the truth intact** rather than by editing the test. That test is no
longer `#[ignore]`d - it was promoted to a permanent assertion, so the day someone fixes
`@font-face` without updating the registry, it fails. That is the direction of failure worth
having.

The two notes for `css.font-face` and `css.animations` are now stale in one respect - "no
diagnostic is reported" is false as of S4. Their `Missing` status is still correct.

## S32 - The name table claims globals that do not exist, and `DOMException` makes every Web API error the wrong type

Two findings from the round that closed most of S10. Both are classes rather than incidents, which
is what makes them worth their own entries.

### The false-claim class

`String.prototype.codePointAt` is **absent** - and it **is** present in the global name table at
`crates/render-js/src/value.rs:2795`, but is never installed on any path.

**A name table listing things which do not exist is worse than no name table.** It makes the
capability look present to anyone reading the code, to any tool scanning for surface area, and to
any agent dispatched later with a brief that says "the platform surface is mostly there". That last
failure has already happened in this project: an agent was briefed that `TextEncoder`/`DataView`
were "an honest absence" while a name table implied otherwise.

At least three claim sites exist - a global name table, an `ObjectHost` enum, and a
`NativeFunction` enum - and which entries are unreachable in each is now being enumerated and
grouped by cause: claimed but never wired, wired behind a condition that can never be true, and
genuinely dead left over from a removed feature.

**Nothing gets deleted.** A dead enum variant is not a gap in the engine, it is a *claim*, and
removing a claim is legitimate where removing a feature is not. The project's rule is that gaps
move forward; making a false claim honest is not moving a gap anywhere.

### `DOMException` is absent, so every Web API error is the wrong type

`DataCloneError` had to be implemented as a plain `Error` with `name === "DataCloneError"` "because
this engine has no `DOMException`". That generalises: **`DataCloneError`, selector `SyntaxError`,
`QuotaExceededError`, `InvalidStateError`, `NotFoundError`, `AbortError` and `SecurityError` are
all currently plain `Error`s.**

The consequence is not cosmetic. The common idioms are `e instanceof DOMException` and
`e.name === "NotFoundError"`, and **`instanceof DOMException` is false for every Web API error this
engine raises.** Real code branches on exactly that.

So this needs the interface per WebIDL - `name` per error type, the legacy `code` member, the
standard members, correct `instanceof` behaviour, and the fact that its heritage is `Error` while
the specification's own heritage rules still do not make it an `Error` - and then an **enumerated**
decision per error type about migrating. Half of them silently left as `Error` would be the
failure mode; an explicit list with reasons is fine.

### Also closed this round, with expectations measured against Node first

- **`TextEncoder`/`TextDecoder`** - a lone surrogate in either half becomes U+FFFD rather than
  CESU-8; one error per **sequence** for a truncation; overlong `[0xC0,0xAF]` gives **two** U+FFFD;
  a surrogate sequence gives **three**; `[0xF5,...]` gives **four**; a bad continuation
  re-synchronises. Streaming holds a partial sequence back, survives a further empty
  `{stream:true}` call, and only errors at end of stream - where `fatal` sees it. `encodeInto`'s
  `read` counts **UTF-16 units**. `latin1`, `iso-8859-1`, `ascii` and `us-ascii` all resolve to
  **windows-1252**, and `x-user-defined` maps `0x80`-`0x9F` to U+F780-U+F7FF.
- **`DataView`, plus `ArrayBuffer`** - the second was added although it was not on the list,
  because Node shows `new DataView(typedArray)` is a `TypeError`, so `DataView` without
  `ArrayBuffer` would be "a global that exists and cannot be used". **That is exactly the rule this
  project works to.** The key correction: **`DataView`'s constructor takes three parameters and
  there is no constructor-level endianness** - Node confirms a fourth argument is ignored - so the
  default is big-endian and every accessor carries its own `littleEndian`.
- **`structuredClone`** - deep copies with distinct identity at every level, and **cycles and
  repeated references survive** via a memo (`copy.self === copy`, `copy.a === copy.b`). A function,
  symbol, promise, `WeakMap`, `Proxy` or DOM node throws `DataCloneError`; a symbol *key* is
  skipped without error, matching the algorithm's "own enumerable string-keyed properties".

Two real bugs were caught by the new tests and fixed: a UTF-8 scalar accumulator that dropped the
low 6 bits, and a windows-1252 table wrongly applied above `0x9F` (index out of bounds on `0xE9`).

### Honest limits, recorded so they are not mistaken for oversights

- A buffer is **byte-granular** (one slot per byte), so `Int32Array(buffer)` is **refused with a
  `TypeError` naming the reason** rather than answered with a wrong number. Doing it properly means
  changing every typed-array read/write path that 237 tests depend on. **Refusing is correct.**
- No `DOMException` - above.

### The two JS probe blockers are gone

**The qq bundle completes**, so it has no first-failure byte offset; the measurable is its step
count: 273,779 -> 274,155 -> **274,498** with `TextEncoder`/`TextDecoder`, **274,498** with
`DataView`/`ArrayBuffer` (a change of **zero**), 274,496 with `structuredClone` (**minus two**).
The last two are recorded as real results: the bundle's bootstrap does not reach them, so landing
them cost it nothing and gained it nothing. Only the text codecs moved it, consistently with
bundled base64/utf8 helpers being on its path.

**The bilibili probe no longer stops at `Proxy is not defined`** - that blocker is long gone. It now
runs to completion with **0 threw**, and builds real Vue 3 output: `data-v-*` scope attributes, a
populated feed tree down to `DIV.recommended-container_floor-aside > DIV.feed2 >
MAIN.bili-feed4-layout`. One informational line reports no manifest entry for a CDN script, which
is a corpus gap rather than an engine failure.

**So there is no next JavaScript blocker to report, because there is no first failure.** That is a
change of kind, not of degree: for the whole of this project the JS story was "it stops at X", and
it is now "it runs".

## S33 - Corpus sweep: three real parser defects, and a proven blind spot in the corpus itself

A sweep of **3,718 real documents** - 16 captured production pages (Chinese portals, Wikipedia data
tables, an IANA media-types table, a government statistics page, a NASA page, two login pages) plus
**3,679 markup fragments and 23 element spans extracted verbatim from 93 shipped JavaScript
bundles**: real client-side templates, Sketch-exported SVG icons, `foreignObject` feature
detectors, Handlebars templates.

### Every diagnostic in the corpus is the site's own markup, not ours

| kind | count (all 3,718) | classification |
| --- | --- | --- |
| `unknown-named-character-reference` | 5,574 | site's own - unencoded `&` in query strings. Specification-mandated; every browser reports it |
| `missing-whitespace-between-attributes` | 837 | site's own - verified **byte level**: `rel="noreferrer"class="..."`, `content="..."href="..."`. A minifier dropped the separator |
| `duplicate-attribute` | 98 | site's own - `<a class="icon" class="/article/1">` |
| `unexpected-token` | 110 | site's own - `</a></a>`, `<body>` after `</head>`, a doubled `</script>` |
| `unexpected-character-in-unquoted-attribute-value` | 60 | site's own - JavaScript array literals parsed as markup |
| the remaining five kinds | 13 | site's own, each traced to its source |

**The important part: the positions were verified, not just the kinds.** That is the S25 class of
defect - a diagnostic that fires on correct markup - and `missing-whitespace-between-attributes`
with 837 hits was the one that looked like a false positive. **It is not one: the separator really
is absent.**

### Three real defects, all fixed at the mechanism

1. **Raw-text serialisation escaped five elements it should not have.** `RAW_TEXT_ELEMENTS` was
   `{script, style}`; the specification's list is `style, script, xmp, iframe, noembed, noframes,
   plaintext`, **plus `noscript` when scripting is enabled**. Escaping a raw text element's children
   produces markup that does not reparse to the same text, so serialisation was not a fixed point.
   **Two real pages hit it**: `<noscript><meta http-equiv="refresh" .../></noscript>` grew by 8
   characters *on every pass, unbounded*. The decision also moved to **per text node**, because the
   specification decides by the text node's **parent** - so a second text node appended by script
   must also serialise literally. The scripting flag is threaded through
   `serialize_html_*_with_scripting`, mirroring `parse_document_with_scripting`, since `noscript`
   is the one element whose serialisation depends on it and **the two modes are indistinguishable
   in the tree**.
2. **`duplicate-attribute` pointed past the end of the tag.** It used the tokenizer's offset
   *after* `consume_attribute`, which for a duplicate that happens to be the tag's last attribute
   is past the `>`. The corpus hit this 98 times; **an offset landing on the whitespace before `>`
   names none of the markup that caused it.** Now reported at the attribute's own start, the same
   place S25's fix points.
3. ~~**A bogus comment ended at `>` instead of `-->`.** `consume_bogus_comment` scanned to the next
   `>`; the specification's bogus comment state switches to the **comment state** on any other first
   character, so the comment runs to `-->` or end of input.~~ **THIS ENTRY WAS WRONG AND THE "FIX"
   WAS A REGRESSION. See the correction below.** The engine was already correct: the **current**
   specification (13.2.5.41) says a bogus comment appends every character and **stays in that
   state**, ending at the next `>`. Scanning to `-->` is the **superseded 2011 rule**, and the
   html5lib reference suite agrees with the current text in 58 cases in
   `processing-instructions.dat` alone. The change was reverted and the four tests that encoded it
   were corrected.

   **The 714,283 figure I reported as a triumph was an artefact of that regression.** Under the
   engine's actual, correct behaviour the comment on that page stays 92 characters. I wrote the
   number into this register and described it as "the classic real failure mode of a missing
   `<script>` wrapper" - **it was a self-inflicted regression that I then reported as a find.**

### The correction, and what it cost

An external suite found what two rounds of internal review could not: **the spec had been read
backwards, and in the direction that made a broken engine look fixed.** The two failures compounded
exactly as this project's history predicts:

1. A reading of the specification was wrong, and the implementation was changed to match it.
2. The change was verified by a round that built its own trimmed fixtures from the *new* behaviour,
   so the fixtures and the code agreed and nothing contradicted either.
3. **I then wrote the resulting number into the register and presented it as evidence of progress.**

The generalisation, and it is the most expensive lesson in this register: **a specification reading
verified only against code you just changed is not verified.** The check has to be an authority the
implementation cannot influence. That is the entire argument for the external suite, and it is why
`tools/html5lib/` now exists.

### The number that came out of it

`cargo run -p render-html --example html5lib`, over **7,377 (test, scripting-mode) pairs** at two
pinned revisions:

| | cases | share of 7,377 |
| --- | --- | --- |
| **pass** | **5,702** | **77.29%** |
| acceptable difference | 190 | 2.58% |
| unimplemented feature | 776 | 10.52% |
| harness limitation | 0 | 0.00% |
| **engine defect** | **709** | **9.61%** |

**86.38% of the 6,601 pairs** that are neither unimplemented nor harness-limited. The denominator is
pairs, not tests: a test with neither `#script-on` nor `#script-off` runs in **both** scripting
modes. Both modes are reported, and they are identical for every mechanism except one, so nothing
rests on a single mode.

**A separate claim that is deliberately not folded in:** the suite's parse-error *count* agrees on
only **2,666 of 6,601 (40.39%)** - the engine **under-reports on 3,100 and over-reports on 835.**
That is a real conformance gap against §13.2.2, and a reader of the tree figure alone could not see
it: **a parser can build the right tree and still report the wrong number of errors.**

### Seven mechanisms fixed, seven deliberately left

Fixed, each with a committed trimmed fixture guarding the **mechanism** and never the score: the
**frameset insertion modes** and the `frameset-ok` flag at all twenty-odd spec sites (230 cases);
**processing-instruction tokens** with `xml` resolving to a comment (253); the **script data state
machine**, all seventeen states (~800); `rb`/`rtc`/`rp`/`rt` implied end tags (48); **end of file
inside a template** still creating the body (40); the bogus comment rule, reverted; and
`frameset-ok` in foreign content and on the start tags that reset it.

**Left open, with the mechanism named:** the **adoption agency** (~40 cases - the most intricate and
the most worth doing next), `<p>` in foreign content and in `<button>` (48), foster parenting of
`<input>` and text (~20), `--!>` as a comment end (6), `</` followed by a non-letter (4), `<table>`
placement, and 216 fragment-parsing cases that need the §13.4 algorithm.

**The remaining tail is flat:** after the fixes no engine-defect mechanism accounts for more than 24
cases. That is the shape you get when the causes are distinct bugs rather than one shared cause, and
it means **there is no single high-leverage fix left in the parser.**

### Two runner defects that were inflating the first measurement

Recorded because they are the failure this project keeps meeting, in the instrument rather than the
engine: the `.dat` reader **split on newlines**, and the format does not escape newlines, so a text
node holding one spans several physical lines and reading it a line at a time truncated the value;
and the DOM dumper **mis-formatted attribute lines**, putting 466 cases into an attribute-value
mechanism that was entirely the harness's fault. The first number printed was wrong twice over.

The negative control caught both classes before they reached a report, and **its first version was
also wrong**: it counted mutated cases that came out non-pass, and a text perturbation scored 128/128
"detected" because they were already failing for another reason - **which would have passed with a
comparison that never looked at a text node.** It now counts only pass-to-non-pass transitions, and
`drop-last-line` is the strong evidence: it changed 6,921 cases, 5,702 of which were passing, and
**all 5,702 stopped passing.**

### A trap worth writing down for the next fetcher

**Do not pin `html5lib-tests` HEAD.** Its HEAD commit is titled *"Tree construction tests have moved
to WPT"* and **deletes** `tree-construction/`, `tokenizer/`, `serializer/` and `encoding/`. **Pinning
HEAD fetches an empty corpus and reports a vacuous 100%.** The pin is the parent, `9329e64` - the
last revision that still contains them - plus the WPT revision, whose copy of the same files is a
strict superset.

**After the fixes, 3,718 of 3,718 documents are stable**, including a second pass with scripting
disabled, plus 363 foreign and template subtrees taken on their own. Zero tree-invariant failures
across single-parent, parent/children agreement, acyclicity, one `html`/`head`/`body`, doctype
before `html`, no `a`-in-`a` / `button`-in-`button` / `form`-in-`form` / `p`-in-`p` /
`nobr`-in-`nobr`, all HTML names lowercase, table sections in their only legal parents, and no
foreign element without a foreign root.

**13 committed fixtures** reduce the corpus classes into the repo, and **no test reads `.diag/`** -
that directory is gitignored and absent from a clean checkout, so such a test would be dead weight.

### The three audits, and one of them is a proven blind spot

- **Foreign content - clean, and better evidenced than before.** The corpus has 347 `svg` roots,
  186 `circle`, 328 `path`, 111 `g`, plus `clipPath`/`feColorMatrix`/`filter`/`mask`/`use`, and 8
  `math`. Attribute namespaces line up: 327 `xmlns` in the XMLNS namespace with no prefix, 99
  `xmlns:xlink` in XMLNS with the `xmlns` prefix, 46 `xml:space` in XML with the `xml` prefix, 2
  `xlink:href` in XLink. **Real markup exercises three of the four namespaces**; the only case not
  covered is an attribute with **no** namespace on a foreign element. `foreignObject` with HTML
  inside is correct, including that the inner `xmlns` is *not* namespaced - because the start tag
  went through the "in body" rules.
- **`<template>` - passes, with a finding.** Only 4 in 3,718 documents, all on one login page, and
  real usage is a client-side **markup** template rather than a row template: **no `<template>` in
  the corpus contains `<tr>`, `<td>` or `<option>`.** What it does rely on, beyond inertness and
  parentlessness, is that the contents hold fully namespaced foreign content and that `{{ }}`
  placeholders survive as text in both text nodes and attribute values. Both hold - but **the `in
  template` insertion mode's row handling is unexercised by real input.**
- **Adoption agency - the corpus cannot exercise it, and that was proved rather than assumed.** A
  stack-based detector over all 3,718 documents finds **zero** cases of a formatting element's end
  tag issued while a block element is open above it. The detector was validated by running it
  against the specification's own worked examples plus `<b><table><td>...</b>`, where it caught all
  four shapes. The only agency activity in the corpus is three `</a></a>` pairs, which take the "no
  such element, any other end tag" path.

  Its conclusion: **server-rendered HTML is well-nested by construction, so the untidy case this
  algorithm exists for does not exist in real pages.** The three specification examples remain the
  only coverage of the most intricate part of HTML tree construction, and **no amount of additional
  real-page capture will change that.**

### The negative control is what made the sweep falsifiable

Before trusting any output, a DOM was built through `render-dom`'s API - **bypassing the parser** -
with `a`-in-`a`, `p`-in-`p`, a `td` outside a `tr`, and a foreign element with no foreign root. The
checker trips all four. Without it, "zero invariant failures" would have been **unfalsifiable** -
which is the trap that has now been sprung three times in this project.

It also paid for itself immediately: the control **caught two bugs in the checker's own
implementation** - an empty allow-list that flagged every element, and a `has_foreign_root` test
made vacuous by an `||` - that would have produced **19,800 phantom findings**.

## S34 - There is no line-breaking algorithm at all, and the intrinsic-width path assumes whitespace exists

Found by grepping for something nobody had looked at, the same way S24 (form owner) was found. It
is recorded here at the top of the register because **it lands on the language this project's entire
corpus is written in**, and because the second half is a plain bug rather than a missing feature.

### Every character is a wrap opportunity

`crates/render-layout/src/solver/inline.rs:1192`: text is exploded with `for character in
text.chars()`, and each character's `wrap_allowed` comes from nothing but `white-space` being
`nowrap` or not (`inline.rs:1187-1191`). There is no break-opportunity computation anywhere - the
Unicode line breaking algorithm (UAX #14) has zero occurrences in the tree, as do `kinsoku`,
`LineBreak`, UAX #9 and bidi.

What that costs, stated per script:

- **CJK: wrapping is right by accident.** Breaking between any two Han characters is correct, so
  greedy filling produces acceptable line breaks for Chinese text. The engine gets this for the
  wrong reason, which means it will get it wrong the moment the wrong reason stops holding - for
  example at a mixed Han/Latin boundary, or inside a run containing punctuation.
- **CJK: kinsoku shori is absent, and this is a visible defect.** No forbidden line-start or
  line-end characters are respected, so closing brackets, full stops, commas and other trailing
  punctuation can begin a line, and opening brackets can end one. On a Chinese page that reads as
  broken typesetting, and it is the single most recognisable CJK layout error there is.
- **Latin: long words break mid-word.** Greedy filling means ordinary text happens to break at
  spaces, so this is invisible until a single word exceeds the line - at which point browsers with
  `overflow-wrap: normal` would have overflowed instead. The engine breaks instead, and it also
  breaks inside a word when hyphenation should apply and does not.

### The intrinsic-width path is a plain bug for unspaced text

`crates/render-layout/src/solver/inline.rs:1495`: `min_content_width` computes the widest of
`text.split_whitespace()`. **CJK text has no whitespace**, so an unspaced Chinese run is treated as
**one unbreakable word** and its min-content width is the width of the entire run.

CSS 2.1 §10.3.5 defines min-content as the widest unbreakable run, and for unspaced scripts the
unbreakable run is a single character, not a whole paragraph. So **any Chinese element with an
intrinsic width - `width: min-content`, `width: max-content`, a shrink-to-fit float, an auto-width
table column, a flex item - gets a width derived from the whole paragraph instead of from one
character.** That is a wrong used value on a public geometry helper, and intrinsic widths feed
tables, floats, inline-blocks and the sticky constraint alike.

This is the same class as the phantom right margin found in the same crate recently: a fall-through
arm doing the wrong thing for a case the author did not consider, visible only on the script the
test corpus is not written in.

### What implementing it actually requires

Not "add UAX #14". Three separable pieces, and the order matters because the first is a bug fix
and the other two are features:

1. **Fix `min_content_width` for unspaced runs** - treat a run with no break opportunity as
   per-character rather than whole-run. Small, self-contained, and it is a correctness fix rather
   than a feature.
2. **A break-opportunity computation** replacing the per-character default, implementing the line
   breaking algorithm's interaction classes at least for the scripts in play. `white-space`,
   `word-break` (`normal` / `break-all` / `keep-all`) and `line-break` (`strict` / `loose` / `anywhere`)
   all feed it, and all three are currently inert except `nowrap`.
3. **Kinsoku shori** - forbidden line-start and line-end classes, which is what makes Chinese
   typesetting correct rather than merely non-overflowing.

There is also **no bidi at all** (UAX #9, zero occurrences), so right-to-left scripts are laid out
left-to-right. That is a separate and larger item, and given the corpus it is correctly lower
priority than the three above - but it should be recorded rather than rediscovered.

**Untested either way**: this is the kind of gap whose absence is invisible until a page in the
target script is rendered, and the acceptance harness's five fixtures do not currently include an
unspaced CJK paragraph in an intrinsically-sized box.

## S35 - `scripting_enabled` is a flag that cannot be validated, and flipping it produces exactly the bare-text symptom still being chased

Found while closing S29's unpinned-behaviour round and asked about as a side question ("is the UA
sheet the same on the browser path and the offline path?"). The answer is **yes for everything
except one field, and that one field is the strongest remaining lead for S28.**

### What is safe, and why

There is **one** construction site in the workspace: `ua_style_sheet(quirks_mode,
scripting_enabled)` in `crates/render-core/src/document.rs:363`, called once at `document.rs:1197`.
The browser path reaches it through
`Document::render_with_external_style_sheets_and_images` - **the same entry point** - so **no
second copy of the sheet exists and none can drift.**

And `quirks_mode` rides on the `Document` itself (`document.rs:970`, read at
`render_worker.rs:508`), so **it cannot diverge.** The parity question has a clean answer for
everything except one field.

### What is not safe

**`scripting_enabled` is a `DocumentRenderOptions` field, defaulting to `true`, that the caller must
keep in step with the parse mode - and `Document` does not record its own scripting mode, so it
*cannot detect a mismatch*.** The only guarantee is three doc comments.

Why that matters, concretely: **the `noscript { display: none }` rule leaves the sheet when
scripting is disabled.** Flip the flag and **every `noscript` subtree becomes live content**. And a
`noscript` element **parsed as scripting-enabled is one inert text node** - so the page shows its
fallback markup as raw text.

**That is character-for-character the symptom S28 has been chasing for three rounds.**

The browser path is right by construction today, so this is a **latent hazard rather than the
active cause** - and saying so is the point. But it is exactly the shape of thing that produces
this class of symptom later: a boolean with no invariant, a default, and no check. Any future path
that renders a document parsed one way with options built the other way gets bare text, and nothing
in the system objects.

**The fix is not to check the flag at runtime** - it is to make the mismatch unrepresentable.
`scripting_mode` belongs on the `Document` beside `quirks_mode`, so that the options cannot
disagree with the parse, and so that the sheet's `noscript` branch is derived rather than supplied.

### A second finding: the measurement probe diverged, and it was the probe

`crates/render-layout/examples/vanish_probe.rs:152` **reads the UA sheet out of a file**
(`parse_stylesheet(&fs::read_to_string(ua_path).unwrap_or_default())`) because `render-layout` does
not depend on `render-core` and the constructor is private.

Consequences, and they matter for reading earlier numbers:

- **Omitting `--ua` yields an empty sheet.** So the 4/122 run recorded in S28 was a **probe
  artefact**, and the probe is structurally unable to reproduce the quirks or `noscript` variants at
  all - it has no `quirks_mode` and no `scripting_enabled`.
- When supplied, the sheet is trusted verbatim, so **it can be stale or hand-edited** and will never
  carry the quirks or `noscript` branches unless the exporter included them.

So any number produced by that probe is a measurement **of a different configuration than the
browser uses**, and should be read that way. Its DOM parse does match the browser - checked, not
assumed.

## S29 addendum: all four surviving mutations are now killed

Verified by mutation rather than by the existence of a test. **Final battery: 11 mutations, 0
survivors.** `render-layout` 202 -> **216**, `render-css` 183 -> **184**, no pre-existing test
changed.

| mutation | killed by | proof |
| --- | --- | --- |
| `table.rs:821` `columns.saturating_add(1)` -> `columns` | `a_fixed_layout_table_reserves_a_border_spacing_at_each_table_edge` (**only** failing test) | 200px table, 2 columns, `border-spacing: 7px`, first cell `width: 40px`; the auto column must be 200 - 3x7 - 40 = **139**, the mutant gives 146 |
| `table.rs:173` comparing `(prec, width)` instead of `(width, prec)` | `border_width_is_compared_before_border_style` (**only**) | 1px `double` beside 3px `dotted` - width decides |
| `style_precedence` `Dashed`/`Dotted` swapped | `equal_widths_are_decided_by_the_border_style_precedence_order` (**only**, 5 style pairs, both directions) | equal-width ties only |
| `collapsed_border_winner` -> first-claim-wins | 7 tests, incl. `the_wider_border_wins_a_shared_line_even_when_it_is_the_second_claim` | 1px claim first, 3px claim second |
| `sticky.rs:162` end-axis arm inverted, and -> `0.0` | **exactly** the two new tests: `a_bottom_inset_holds_the_sticky_box_above_the_bottom_of_the_scrollport`, `a_right_inset_holds_the_sticky_box_against_the_right_of_the_scrollport` | footer offset -850, painted y 550 (= 600 - 50); rail painted x 280 (= 400 - 120) |
| `cascade.rs:1102` the omitted `line` slot's initial -> `underline` | `a_shorthand_that_omits_the_line_slot_writes_the_line_initial` (**only**) | the key: `text-decoration: none` does **not** reach the fallback, because `none` is itself a `text-decoration-line` value; `solid` / `2px` / `blue` do |

### §17.6.2: the resolution code was already correct - and two adjacent gaps turned up

`outranks` is the specification verbatim: hidden first, then `(width, style_precedence)` compared
width-first (which gets the second rule free, because `None` has the lowest precedence), and
`style_precedence` is the specification's own list in order. **So the audit's finding was right and
the code was right - the tests were the whole gap.**

Two things the mutation audit could not see:

- **Rule 4 is absent and cannot be tested as written.** `collapse_table_borders`
  (`table.rs:1144-1190`) collects claims only from cells and the table; **rows, row groups, columns
  and column groups make no claims at all**, so
  `cell > row > row group > column > column group > table` has nothing to order. **Missing, not
  wrong** - and the honest way to state it, because "untested" and "not implemented" are different
  findings and the second one was previously invisible.
- **`table: bool` is a real deviation from rule 3.** `outranks` returns `self.table` on a
  table-ness difference, so **the table wins the outer grid lines regardless of width**, and
  §17.6.2.1 has no such exception - the table is only *last* in rule 4's colour-only tie. So
  `table { border: 1px }` beside `td { border: 8px }` resolves wrong by the letter of the
  specification. **It was deliberately not changed:** it is a load-bearing design decision,
  `the_table_border_wins_the_outer_grid_lines` is satisfied by width too, and this round was scoped
  to unpinned behaviour. **Reported rather than silently rewritten.**

### §9.2.1.1: the full split, and the detail that made it work

Implemented as a **post-pass** over the finished tree, which is forced: a split needs **both** sides
of the break, and content after the block is unknown until the inline's children are all appended.

The detail that made it work, and that the first attempt got wrong:

> The engine's own `AnonymousBlock` wrappers are part of the chain, not incidental.
> `layout_block_like` routes an `AnonymousBlock` to the inline path unconditionally, so a block
> placed *inside* one is walked past by `collect_inline_atoms` and dissolves exactly as before.
> **Lifting it *beside* the anonymous block** is what makes it reach `layout_block`. My first
> attempt got this wrong and the block vanished again.

Geometry now asserted, with the control being the same class as a direct child of a block parent:

- the span's box at **(0, 19.2) 40x10** - **before the fix it had no fragment at all**
- "before" is a line box at y 0; **"after" is a separate line box at y 29.2, x 0** - it was at x 48,
  beside "before", on the same line
- `body` is **48.4** tall = 19.2 + 10 + 19.2; it was 19.2

`display: contents` is **handled** (`tree.rs:533` re-parents the children and generates no box), and
the new pass does not interfere because a node with no box is not a candidate. That is precisely the
case that makes a caller-side workaround for the S28 counter bug wrong, which is why it was checked.

### A new mutation survivor, found by the agent that introduced the code

Weakening `split_block_in_inlines`' guard from `is_block_container(node)` to
`accepts_inline_children()` leaves **all 216 green**. It survives because in every reachable tree the
parent of a split box is already a block container, so the weaker guard is equivalent.

**So the suite does not distinguish "the block lands beside the anonymous block" from "the block
lands inside it" - the exact failure that made the first attempt dissolve the block again. Do not
relax that guard.** Recorded rather than fixed, because closing it means constructing a tree the
engine cannot currently produce.

### A caveat on the design decision, found by measuring the corpus rather than the code

The decision to derive the owner on every read, and to store exactly one datum, is justified by
**one case: a control owned by a form that is not its ancestor**, which the HTML parser's form
element pointer can create and which nothing in the finished tree says so.

**That case does not occur in any captured page.** A corpus of 27 production pages - plus roughly
110 candidate URLs probed, *including the specification's own forms page* - contains **zero**
`form=` attributes and **zero** nested `<form>` start tags. So:

- the derivation itself is exercised constantly, because almost every control is inside a form, and
- **the one situation the design exists for is exercised by nothing but hand-built trees.**

That is not an argument against the decision, which is still right and still the cheapest
representation that cannot go stale. It is a statement about what kind of evidence exists: the
invariant is **hand-tested and specification-derived, not corpus-validated**, and the next person to
touch form handling should know that rather than assume the corpus had it covered.

It also sharpens what a future capture effort should look for. `form=` and a nested `<form>` are
**rare in the wild** - which is exactly why a corpus built by picking popular pages will never find
it, and why the "no page here has that shape" finding is a statement about the sampling method
rather than about the web.

### Can and cannot

Measured over the combined corpus, with every "cannot" a zero cross-checked by two instruments:

- **can exercise:** S1, S4, S4b, S6 stacking, S8, S10, S13 scroll containers, S14, S16 (mechanism
  only), S19, S22, S26
- **partial:** S5, S6 multi-column, S24
- **cannot:** S7 quirks mode (no doctype-less page), S11 nested browsing contexts (no captured
  iframe `src`), S13 Selection/Range (no interaction recording), S18 device pixel ratio, and S24's
  actual case

And a caution about the shape counts themselves, because the tools that produced them were wrong
first: a page carrying **273 `@font-face` blocks was reported as `webfont_heavy=21`** because the
scanner never looked at the page's own inline CSS, and **every page reported zero inline `<svg>`**
because the counter was never incremented. **Three of that round's four instrument defects failed by
under-reporting**, which is the dangerous direction: a corpus that under-reports looks like a corpus
with few problems.

## S36 - Four defects the new real-page fixtures exposed, and one of them is a check that was passing vacuously

Found by pointing the acceptance harness at four new fixtures chosen to be the shapes the existing
five could not see. The harness is now **56 passed** (from 25), 9 fixtures, **3,348 contract checks**
(from 2,708). S6 and S5 are corrected above; these are the rest.

### A nested scrollport's content inflates the page's scroll width

`FragmentTree::scrollable_content_size` measures **1320 against a 1280 viewport**, so the document
reports `max_scroll_offset().x = 40`. Visible in the same run: a 1300px strip laid out at `x=20`
inside a 1240-wide scroller whose own `max_scroll_offset().x` is 60.

**A scrollport's overflowing content is clipped and must not extend the document.** So a real navbar
that is its own horizontal scroller produces a **spurious horizontal page scrollbar**.

**This is the same defect already recorded in S6** - "a 300px element inflated the document's
scrollable width and produced a spurious scrollbar" - **in a place that fix did not reach.** The
helper was fixed; a second consumer of the same quantity was not.

### `border-collapse: collapse` does not subtract the collapsed border from the column

Measured with a three-cell table, cells declared 100px, table declared 300px:

| stylesheet | table | cells | each cell |
| --- | --- | --- | --- |
| no `border-collapse` | 314 | 314 | 102 @ x=2, 106, 210 |
| `separate` | 314 | 314 | 102 @ x=2, 106, 210 |
| `collapse` | 306 | 306 | 102 @ x=0, 102, 204 |
| `collapse` + `table-layout: fixed` | **300** | **300** | 102 @ x=0, 102, 204 |

(The `+2` per cell is the user-agent sheet's default cell padding - correct, not a defect.)

Under `collapse`, **the collapsed border is subtracted from the cell's position but never from its
width**, so every column is 2px too wide and the table grows by 2px per column. That is the documented
winner-takes-all deviation appearing **in a place the deviation note does not mention** - and it is
not sub-pixel, it is a per-column error. §17.6.2.1's "a cell's border edge is the grid line" is not
met.

Worse under fixed layout: the **table keeps 300 while its cells total 306, so the cells overflow the
table box.**

**Neither was asserted, because both are defects and a test must never assert a defect.**

### The formatting stage reports `BlockInsideInline` 161 times on a normative table

A fixture built from a real specification page - a `caption`, a `colspan=5` header, a `rowspan=7`
cell, and a `ul` inside a `td` - makes the engine emit **161** `BlockInsideInline` diagnostics.

**161 is not a plausible count for one page's table, and the number is the finding: the table solver
is building inline boxes around table content.**

The first instinct was to assert the formatting stage silent, and the measurement rejected that:
**demanding silence is demanding the engine not report what it can see.** So the count is **pinned**
instead, as a regression detector - a diagnostic appearing is correct; a diagnostic going missing is
the worst failure available.

### A check in the harness itself was passing vacuously on all five original fixtures

`inspect::lines_in_reading_order` ranked **elements only**, then looked up a text fragment's source -
which is a **text node**. Every line's rank was therefore undefined and the function returned almost
nothing. **The "page text is painted in document order" assertion had been passing on nothing.**

That is the same failure mode as the `scratch_noscript_probe` that inflated a verified test count,
and it is the third time this project has produced a check that cannot fail. The fix ranks the whole
tree, and a **per-fixture `min_order_comparisons` floor** now makes silent reintroduction
impossible.

### Also found

- **The form element pointer works, and the first declaration of that was wrong.** A fixture with a
  `<form>` inside a `<table>` produces exactly the §4.10.18.3 shape: the form stays a child of the
  table, the hidden control inside it is **foster-parented out** to before the table, and **its
  owner is a form that is not its ancestor.** Meanwhile the controls in the table's *cells*
  correctly have **no** owner, because the pointer was cleared by the form's end tag first. **Two
  controls, two mechanisms, two different answers** - which is the design working, on a realistic
  tree, for the case S24 recorded as unexercisable on real pages.
- **Four `UnexpectedToken` parse errors on deliberately malformed markup**, pinned by offset but
  **deliberately not asserted as correct**, because it could not be established that all four are
  required: `</form>` with the form not the current node is a parse error by 13.2.6.4.7, while a
  `<tr>` directly in a table is not obviously one. **Left open and named.**
- **Corruption removed**: `tests/real_site_tasks/tests/zz_probe.rs`, 445 bytes, **every byte
  `0x00`**, written by a shell. Not recoverable; deleted. That is the fourth time this session that
  a shell write has damaged a file.

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
| ~~`AbortController`, `AbortSignal`~~ **now implemented** | `AbortSignal` interface object with `abort`/`timeout`/`any`/`throwIfAborted` is in `crates/render-js/src/prelude.js`. |
| `ResizeObserver` | Component frameworks gate layout on it. |
| `attachShadow`, `ShadowRoot` (and `customElements` for customized built-ins) | Shadow DOM; `docs/generic-browser-todo.md` Priority 3. Autonomous `customElements` now exist (`crates/render-js/src/prelude.js`, reactions hooked from `set_member` in `runtime/eval.rs`); `attributeChangedCallback` does not fire for `classList`/`style`/`dataset`/`Attr.value` writes. |
| `DataView` | Absent even though the whole `TypedArray` family is implemented in `runtime/builtins/typed_array.rs`. A small inconsistency in an otherwise complete area. |
| `WebSocket`, `Worker`, `SharedWorker`, `EventSource`, `BroadcastChannel`, `MessageChannel` | No live-update or off-main-thread execution path. |
| `indexedDB`, `Notification`, `crypto`, `PerformanceObserver`, `WebAssembly`, `ReadableStream`, `TransformStream`, `WeakRef`, `FinalizationRegistry`, `BigInt`, `Atomics` | Lower frequency on ordinary pages. |

A missing global is worse than a wrong one, because it throws a `ReferenceError` at
the point of use and takes the surrounding script with it. Every remaining entry above is a
candidate to check against real-site script corpora already in `.diag/`.

## S11 - No nested browsing contexts, and this is now known to gate **measurement**, not just pages

`iframe` appears in `crates/render-core/src/document.rs:262,269` only as a member of
element classification lists. There is no nested browsing context: no second document,
no separate event loop or task queue, no `postMessage`, no `window.open`. Every
`<iframe>` - embedded video, ad slots, third-party widgets, login frames - renders as
nothing at all. `target="_blank"` and `window.open` are equally absent, so a link that
opens a new context silently does nothing.

### Why this is no longer one gap among twelve

The WPT runner is built, the suite is censusued at **32,576 scored tests across `css/`, `dom/` and
`html/`**, and **it cannot run a single one of them.** WPT's CSS tests are `testcss.js` tests, and
`testcss.js` is an **iframe driver**. `render-core` has no nested browsing contexts.

**The largest area with a real cascade, a real selector matcher and a real layout engine is the one
whose harness cannot run.** The runner reports **no conformance rate at all** - its `Rate` returns
`None` on a zero denominator, so an empty run prints nothing rather than `0%` - and it reports **no
engine defect list**, correctly labelling that "blocked, not empty" rather than "zero defects",
because nothing was evaluated.

So the cost of this row is not one page type. **An engine that cannot host a nested document cannot
be scored by the largest CSS conformance suite in existence**, so the absence costs the project its
ability to *know* whether its CSS is right - which is the measurement this project has been missing
all along, and which two runners are now queued to supply.

Nested browsing contexts alone block **1,772** of the tests that otherwise look statically
executable. That is the number to weight this row by.

### What building it must include, so it is not another DOM-only half

The scope written down for the DOM half (`render-dom` + `render-html`: a nested `Document`,
`srcdoc`, the form element pointer across the boundary) is necessary and **not sufficient**, and the
precedent that makes this explicit is the form owner: that one was delivered correctly as a
relationship plus a written consumption contract, with rendering explicitly reported absent. For
iframes the consuming side is a whole second document going through parse, style, layout and paint -
so the unit is:

- `render-dom` / `render-html`: the nested `Document`, `srcdoc`, `contentDocument`, the parser
  boundary
- `render-core`: a second document through the **whole** pipeline, and the containing block /
  stacking context an iframe establishes
- `render-browser`: the shell side - a second view, its own scroll offset, and its own input
  routing, because an iframe is a nested viewport and not a decoration
- `render-js`: `postMessage`, `window.open`, `target="_blank"`

**A nested document that parses but never paints is worse than none**, because the iframe currently
renders as nothing and would then render as a plausible-looking empty box.

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

## S23 - Closed: `text-decoration: none` was ignored on every page (was the highest user-visible impact)

Reported by direct observation, not found by inspection: pages that remove link underlines
still showed them everywhere. **Fixed - see "Closed" below.** The chain is kept because it is
the clearest example in the project of a bug that lives entirely in the gap between a longhand
and its shorthand.

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

## S5 - **NOT closed.** The rasteriser works; nothing ever calls it

> **Correction.** Like S6, I closed this row on unit tests. The acceptance harness then established
> that **`discover_inline_svgs` and `InlineSvgDiscovery::install` are called from nowhere** - not
> from `Document::render_with_external_style_sheets_and_images`, not from `render-browser`, and not
> from anywhere outside `inline_svg.rs` itself. Verified independently. So the capability is a
> **function nobody calls**, and the registry note claiming the raster "paints through the ordinary
> image command" describes a path that does not exist.

The work is real and correct - it is the *call* that is missing. Both halves of the *implementation*
are done: the parsing half, and the rendering half via the existing
`crates/render-core/src/image/svg.rs` with no new rendering code, through a new
`src/image/inline_svg.rs` that discovers `svg` elements in the `Svg` namespace, serialises the
subtree with the namespace-aware `serialize_html_node`, rasterises, and registers the result as an
image resource.

What it renders, once something calls it: `<rect>`, `<circle>`, `<ellipse>`, `<polygon>`,
`<polyline>`, `<line>`, `path` (`M m L l H h V v C c S s Q q T t A Z z`), `<g>`/`a`/`switch`
transforms, `fill`/`stroke` inheritance, and `viewBox` scaling.

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

## S26 - Closed, and **the defect I recorded here was not the defect**

### What I wrote here before, and why it was wrong

This entry previously read:

> An unparsable declaration discards **itself and every declaration after it in the same
> block** ... The block carrying `display:inline-block` - the rule that keeps the site's top bar
> horizontal - loses it. The list items fall back to the user-agent `list-item`, the header
> stacks vertically, and that is the bare-text screenshot.

**All of that was wrong, and it was wrong in a way I should have caught.** The evidence came from
a browser-side agent reporting a fingerprint over two rules, and I took it as established. When
the fix was implemented, the implementation measured all 134 sites in the corpus and found:

> **0 of the 134 sites cost their block a following declaration. The old recovery boundary was
> already correct - the reporting was not.**

The example in the original report is **self-refuting on inspection**:
`.item{display:inline-block;*display:inline;*zoom:1;min-width:50px;...}` puts
`display:inline-block` **before** the star hack, so even a recovery that discarded everything
after the invalid declaration would have kept it. The declaration the report named as lost could
not have been lost by the mechanism it blamed.

**So the real cause of "styled but bare text" on real pages is still unknown.** See S28. That is
the honest state, and it is worse than the false closure I was about to ship.

### What the real defect was

Purely the **diagnostic position**, not the tree. The recovery boundary is
`Parser::parse_until_before(Delimiter::Semicolon, ...)` - CSS Syntax §5.4.4's "consume a
component value" made literal - and it is **already correct for functions and nested blocks**
because cssparser matches `()`, `[]`, `{}` and function tokens whole inside it. Tested directly:
`*bad: image-set("x;y" 1x); color: red` keeps `color: red`, and
`background: image-set("a;b.png" 1x, url("c}d.png") 2x)` survives intact.

What was broken: the diagnostic pointed at the token **after** the discarded declaration rather
than at the declaration. In the corpus, 134 sites were affected -
`.diag/hao123/assets/inline.css` byte 3119 was reported at 3134 and is now reported at 3119.
Every one of those is a **misreported diagnostic**: a false parse error on every rule containing a
legacy star hack, which is a flood of noise that drowns real errors and misleads any tooling that
counts parse errors - including the acceptance harness, which had to *permit* this diagnostic in
order to assert anything at all.

So the real cost was **diagnostic noise on 134 rules**, not a visibly broken page. That is a much
smaller defect than the one I recorded, and the register had it badly wrong for a day.

### Evidence that nothing was traded away

| | before | after |
| --- | --- | --- |
| declarations reaching the cascade | 42205 | **42205** |
| rules dropped | 0 | **0** |
| declarations dropped | 151 | **151** |
| total diagnostics | 606 | **606** |

The count did not drop **and did not rise**: 97 `unexpected token: Semicolon` plus 37
`unexpected end of input` became 134 `invalid declaration: ...`. A recovery that skips
diagnostics would have shown up here as a drop, and the round's brief made a drop a red flag to
investigate before reporting.

### The invalid-value question, measured rather than assumed

I asked whether an invalid **value** takes the rest of the block with it - which would be the same
bug in a much more common place, since `color: notacolor; margin: 10px` is far more frequent on
the web than a star hack. **The answer is no, and the split is principled:** the syntax layer
stores a value as the token stream it read, so an invalid *value* is a well-formed *declaration*,
and the typed grammar rejects it later. `color: notacolor; margin: 10px; padding: 1px` keeps all
three and emits no syntax diagnostic.

The corpus agrees: 2053 declarations reach the typed stage, 6 are rejected as genuinely invalid,
and the 151 that never reach the cascade are **value truncations inside `var()` and `oklch()`** -
not blocks emptied by a bad property name.

### Two more over-discarding defects, found in the same code path

- **`@font-face` was the only at-rule whose block was read as a declaration list.** A `@page`
  block containing `*zoom: 1` reported one diagnostic about `@page` and **nothing about the bad
  descriptor** - the round-five partial drop, one level over. There is now a
  `DECLARATION_LIST_AT_RULES` table in `at_rules.rs`: 9 at-rules, each with its defining section,
  with a test asserting every entry is recognised, cited and unevaluated.
- **A panic on valid CSS.** `substitute_nesting` in `selector.rs:391` routed
  `QuotedString`/`UnquotedUrl`/`BadUrl` through `parse_nested_block`, which requires a block token
  and **panics** otherwise. `a { b:not("x") { color: red } }` - a nested rule with an ordinary
  attribute selector, **not malformed CSS at all** - took the process down. Fixed by routing them
  to `Literal`, since the tokenizer already consumed their contents and there is nothing to open.
  This is the most serious thing the round found: a crash on correct input, with CSS Nesting
  landing in the same session.

### One fix deliberately not made

`@media;` - a block at-rule written as a statement - reaches a diagnostic arm whose comment
claims it is unreachable. The fix (a `BLOCK_AT_RULES` table plus a variant) was built and then
**removed**, because it changed the message for every recognised at-rule in statement form and
broke a load-bearing test. The table is the right shape; what needs revisiting is the test's
expectation, and that is a scheduling decision, not a unilateral one.

## S29 - Latent defects found by mutation testing: the code is right, and nothing would notice

Found by a read-only audit (`docs/test_quality_audit.md`). **The current code in all three cases
is correct.** The finding is that each is one edit away from being wrong and **no test in the
project would fail** - so these are unpinned behaviour, not live bugs. That distinction matters:
the register should not imply a page is broken today when it is not.

Denominator, stated because a survival rate without one is worse than no rate: **34 hand-picked
mutations attempted across four crates, 24 killed, 10 survived - 29%.** That is not a coverage
number; it is 34 behaviours chosen by hand and weighted towards the newest code. The audit also ran
a **control** for its top finding, which is what makes it credible rather than a list of guesses.

### 1. `table-layout: fixed` with a non-zero `border-spacing` is unpinned

`crates/render-layout/src/solver/table.rs:821` -
`spacing * count_as_f32(columns.saturating_add(1))`. **This is correct.** Changing
`columns.saturating_add(1)` to `columns` leaves **all 47 table tests green**.

**The control is the important part:** the *same* off-by-one in the auto layout path at
`table.rs:731` **is** killed by
`border_spacing_separates_every_column_and_the_table_edges`. So the assertion style is right and
exists - it is simply missing from the fixed-layout branch. That is a hole in one branch, not a
weak test, and it is the cheaper of the two to close.

This matters more than a typical gap: `border-spacing` is non-zero in most real tables, and
`table-layout: fixed` is what data-heavy pages use.

### 2. §17.6.2 border conflict resolution is almost entirely untested

`table.rs:173` and `table.rs:179`. Only the two **trumps** are covered - table beats cell, hidden
beats everything. The rule that actually decides most collapsed-table borders, **"a wider border
beats a narrower one"**, and the whole style-precedence table, have no test. A first-claim-wins
mutation *was* killed, by two tests, so the trumps are pinned.

Untested means: a collapsed table with `1px dashed` beside `2px dotted` can paint the wrong
border down the entire shared line, and nothing would notice. Collapsed tables are everywhere in
real page layout.

### 3. `position: sticky` with `bottom` or `right` has no test at all

`crates/render-layout/src/sticky.rs:162` - the `(None, Some(_))` arm, which is the end-axis
constraint. Inverting it leaves **all 199 `render-layout` tests green**, because **all 12 sticky
tests use `top` or `left`.**

A sticky footer, a sticky right rail, a sticky "back to top" control, a cookie banner pinned to the
bottom - **none of them has any coverage at all.** The implementation looks correct; it is simply
unverified on half its axes.

### 4. The omitted `text-decoration-line` slot is unpinned

`crates/render-css/src/cascade.rs:1102`. The other three initial values are pinned - a
`solid` -> `wavy` mutation dies to four tests - but the **line** slot can be mutated to `underline`
and the whole `render-css` suite stays green.

This is the one that matters most historically: **the original S23 bug was exactly a longhand
losing to another longhand**, and the initial value of the line slot is the value every page that
omits it inherits. The fix is tested; this particular slot's default is not.

### What the audit confirmed is now load-bearing

The assertion whose absence let 92 rules through **exists and is load-bearing**: re-creating the
defect (`Evaluated(false) => apply`) turns **3 tests red**. That is the single most important
confirmation in the audit, because it is the failure mode this project has already shipped once.

### Five test functions that cannot fail

Ten candidates, five real after removing documented skips, nested helper `fn`s, and
"does not panic" tests (where the absence of a panic *is* the assertion):

- `crates/render-js/src/runtime/tests.rs:2199` `temp_diag_mutual_recursion` - **`eprintln!`s only,
  is live, and is counted in the suite. Reads `.diag/bilibili/*`, so it also names a live site in
  source.** This is the same class as the `scratch_noscript_probe` that inflated a count the
  orchestrator had already verified.
- `crates/render-js/src/runtime/tests.rs:2635` `temp_read_5073_state` - same shape, also live.
- `crates/render-js/src/video/present.rs:784` `frame_publication_carries_node_and_url` -
  constructs and drops; cannot fail.
- `tests/real_site_tasks/tests/probe.rs:18` - another agent's file at audit time, header says
  "TEMPORARY ... Deleted before the round closes".
- `tests/real_site_tasks/tests/render_diff.rs:30` - assertion-free **by its own documented
  design**, which is legitimate, but it does mean a crash in the render path is its only possible
  failure mode.

**A gap in the site-neutrality gate falls out of this.** The gate passes clean while a counted test
in `render-js` reads `.diag/bilibili/*`. So the gate's rules do not cover a site name appearing in
a path literal or a test name, only the identifier and comparison forms it was written for. The
gate enforcing the project's hardest rule has a hole in it, and that hole is exactly where a site
name got in.

## S30 - Structural absences: what makes this not yet a browser

Separate from gaps, because these are not defects to be closed one at a time - they are
components a browser has and this does not. This is the honest answer to "how far off is it", and
it is deliberately not expressed as a percentage, because the percentages were refused for cause.

**Verified absent by grep, not inferred:**

| absent | why it matters more than a feature gap |
| --- | --- |
| **ES modules** | **Largely closed.** `render-js` parses import/export into tables (`src/module.rs`) and links them with live bindings (`src/runtime/module.rs`): named/default/namespace imports, re-exports, `export *`, `export * as`, cycles for hoisted declarations, `import.meta.url`, evaluate-once ordering, remembered failures. `render-core/src/module_graph.rs` assembles the graph and `render-browser/src/scripts.rs` fetches it in rounds. **Still open:** dynamic `import()` is a stub that resolves an empty object; top-level `await`; import maps (bare specifiers are diagnosed, not resolved); module code runs sloppy, not strict; a namespace object is a snapshot rather than a live exotic object; classic scripts still skip `import`/`export` instead of throwing a SyntaxError; `type=module` `async` scheduling is treated as defer; `<script type=module>` inserted after load goes through the same path as classic follow-up scripts but is untested in a live window |
| **Web Workers** | zero occurrences of `Worker`. Bundles that offload work get nothing |
| **`window.matchMedia`** | zero occurrences. Every responsive site that gates behaviour in script gets nothing |
| **History API** | `pushState`/`replaceState` change the URL and `history.state` without a load. Back/forward/go within the document fire `popstate` without a load; entries of another document reload. `history.length` reflects the list at each turn's start |
| **WebSocket / `requestIdleCallback` / `PerformanceObserver`** | absent |
| **Iframes** | S11 - no second document, so any page embedding one loses that content entirely |
| **Animations and transitions** | `@keyframes` is parsed, diagnosed, and discarded - 249 blocks in the corpus. No clock, so nothing can be time-driven |
| **Video pixel decode** | S8 - container parsing only |
| **Selection / Range** | S13 |
| **Shadow DOM** | no `customElements` / `attachShadow` |
| **Form submission** | the owner relationship now exists and is derived on read, but there is **no submit event path and no navigation**, so a search box or login does not submit |
| **HTTP/2** | structurally rejected: a pooled connection is removed from the pool while in use and `run()` holds it for the whole request, so one connection carries one in-flight request, and multiplexing is the inverse |
| **Compositing** | there is no compositor. Every frame is a single raster of the whole document, which is why scrolling cannot be cheap and why `position: fixed` cannot be a layer |

**What is genuinely strong**, so the list above is not read as the whole picture: the HTML tree
builder (120 tests, foreign content, template inertness, the adoption agency with the spec's own
worked examples, form owner derived on read); the CSS cascade and selector matcher; table layout
(47 tests, CSS 2.1 §17 including border-collapse conflict resolution, row groups, `colspan`/
`rowspan`, fixed layout); the font axis and font matching (a pure function over a face table, 42
tests, synthetic bold and oblique); network phase budgets and connection reuse; and a JS engine
that completes a 2.27 MB production bundle.

**The shape of the remaining distance is not a long tail.** It is roughly a dozen architectural
absences above, of which ES modules, `matchMedia` and form submission are the three that would
break the largest number of real pages per unit of work.

## S28 - OPEN: "styled but bare text" on a real page - the cause is still unknown

This is the project's most-reported symptom and, as of S26, **unexplained**. Recording it
separately so it cannot be closed by accident again.

### Update: the `vanished` lead was a counter bug, and the box tree is correct

The lead this item was opened on is **resolved, and it was the instrument that was wrong, not the
engine.** `crates/render-browser/src/render_worker.rs:417-433` classifies on an element's **own**
computed `display` and never consults the ancestor chain, so **every descendant of a `display:
none` container reads as "vanished"**. The counter's `display: none` exclusion is scoped to one
node when it should be scoped to the subtree.

**All 151 accounted for, zero unexplained** (1280x600, real document plus its three real
stylesheets):

| hidden ancestor | declaration | n |
| --- | --- | --- |
| `div.service_container.fs_mod` | `jd_first.css` #301/#310 `display:none`, plus `@media (max-width:1679px){...!important}` | 63 |
| `div#J_cate.cate` | `jd_index.css` #1311 `display:none` | 41 |
| `div#navitems` | the document's own `style="display: none;"` | 30 |
| `div#J_focus.focus` | `jd_first.css` #283 `display:none` | 7 |
| `div#settleup.dropdown` | `jd_first.css` #235 `display:none` | 4 |
| `div.J_tab_content.service_pop` | `jd_first.css` #612 `display:none` | 4 |
| `div#J_sideslider` | `jd_first.css` #591 `display:none` | 2 |

**Every one is specified behaviour.** The styled frame is correct: 266 of 650 elements have boxes
and 407 sit inside containers the author deliberately hides. The unstyled frame's 20 are all under
`#navitems`. Both buckets in the earlier version of this item - "a matching `display` exists yet no
box" and "no author `display` matches" - **are not two bugs; they are one miscount.** In both cases
the first divergence is the same line, `render-layout/src/tree.rs:511-513`, where
`append_dom_node` returns early on a `display: none` ancestor. That is correct per CSS Display 3
§2.1, and it was proved causally: adding `div#J_cate{display:block}` makes all 20 of the
`i.cate_menu_icon` instances produce fragments at their declared 14x14, and the `inline-block` path
is correct.

A detail worth keeping, because it invalidates naive comparison: **without the user-agent sheet the
counts are 4 and 122, not 20 and 151.** `.cw-icon`, `.dropdown-layer` and `#J_cart_pop` are only
block-level *because the UA sheet says so*. So any divergence between the browser path's UA sheet
and the offline path's would change real rendering while being invisible in these counts - now
being checked.

**The paradox is resolved too.** Adding a stylesheet *increases* vanished elements because the
stylesheet legitimately sets `display: none` on six containers that were visible before, so
elements under a hidden ancestor go 169 -> 407. Mechanism "a box is being dropped" is **zero**.

**The symptom itself remains unexplained**, and the lead has moved: the remaining candidate is the
DOM **after script execution**, which is `render-js`/`render-core`. The recommended next step, from
the agent that closed this lead, is the live document's post-script DOM compared **box-for-box
against a real browser** rather than anything in the cascade or the solver. The register's own
earlier finding supports that: the browser path and the offline path are item-for-item identical,
so the difference must be in what script did to the DOM.

## S31 - CSS 2.1 §9.2.1.1 block-in-inline is diagnosed and then silently dissolves the box

Found by measurement while closing S28's counter bug, in `crates/render-layout`, and **deliberately
not fixed by the agent that found it** - correctly, because the fix is larger than the discovery.

Minimal repro:

```css
p { display: inline }
.box { display: block; width: 40px; height: 10px }
```

```html
<p>before<span class="box"></span>after</p>
```

The span gets a `block/Block` formatting node with `has_box=false`, while **the same class as a
direct child of a block parent does get a box**. So this is a **wrong layout, not a missing one** -
which is the worse of the two, because the diagnostic makes it findable while the result is silently
incorrect.

Traced through three places:

- `tree.rs:98-106` - `Inline::accepts_inline_children()` returns false
- `tree.rs:586-596` - the block is appended **directly under the `Inline` node**, emitting the
  `BlockInsideInline` diagnostic
- **`inline.rs:1247-1249`**, the actual cause: `collect_inline_atoms` recurses into **any**
  non-text / non-break / non-atomic node's children, so the block container is walked straight past
  and `layout_block` is **never called**. Its children are re-parented into the inline run.

§9.2.1.1 requires two things, and a fix that does only the first produces wrong geometry rather
than none: **lift the block to the nearest block container**, *and* **split the enclosing inline
element's own box in two** with the block between the halves. That touches child ordering in
`tree.rs`, `is_first_child`, and the inline solver's intrinsic-width recursion. The diagnostic must
keep firing.

Also open from the same investigation: **`display: contents` is unverified.** Nobody has checked
whether it is handled, and it is one of the two cases where a caller-side workaround for the S28
counter bug would be wrong.

### The original investigation, kept because the eliminations are the result

This is the project's most-reported symptom and, as of S26, **unexplained**. Recording it
separately so it cannot be closed by accident again.

### What has been eliminated, by measurement

- **Not the browser's style pipeline.** The apply-to-commit path is item-for-item equivalent to
  handing the engine the sheets directly: `stylesheets=3 fragments=492 items=349
  content_height=3028` both ways. `merge_current_style_sheets` loses nothing, and the re-plan and
  rematch path produces the same keys.
- **Not inline-script DOM mutation.** Replaying the inline scripts and re-rendering offline
  produced a byte-identical result.
- **Not the CSS parser's error recovery.** See S26 - measured across all 134 sites in the corpus,
  zero of them lost a declaration.
- **Not a media-query eligibility failure.** That one was real and is fixed (S17), but it is not
  this symptom.

### The live lead

Per-frame instrumentation counting elements with a non-`none` display that produce **no box at
all** - `vanished` - goes from **20 on an unstyled frame to 151 on a styled one**. That is the
signal, and it splits into two buckets that have not been separated:

- **(a) A matching `display` exists and is right, yet no box is produced.** Named cases:
  `display: inline-block` from `first-screen.chunk.css` rule #319 on `.cate_menu_icon`, and
  `display: block` from `index.chunk.css` rule #1532 on `span.loading`. **This is
  `render-layout`** - the rule matched, the value is correct, and the box does not exist.
- **(b) No author `display` matches at all**, for around 151 block-level elements including
  `.cw-icon`, `.dropdown-layer`, `#J_cart_pop`, `.JS_navCtn.cate_menu` and `.cate_menu_item`.
  **This is `render-css`** - selector matching or the cascade.

Bucket (a) is the more suspicious of the two, because "the rule matched and the value is correct"
should mean a box, and the fact that it does not is a contradiction rather than a missing feature.

**How to proceed:** reproduce through the local HTTP fixture - the real document plus its three
real stylesheets, served locally, which is known to reproduce the symptom through the full
navigation path - and bisect the two buckets separately. Instrument the *specific* element rather
than the page, and find the first point in layout where a box that should exist does not.
**Do not accept a fix that makes `vanished` smaller without establishing why each element
vanished** - a smaller counter with an unexplained residue is the failure mode this entry has
already been written up for once.

### A hypothesis this entry has not yet tested, and it would explain everything

**The screenshot that established this symptom may itself be a frame that was never committed.**

The screenshot was captured through the same browser path whose logging was later found to run
`log_completed_frame_debug` **before** the identity gate in `commit_render` - so a frame that was
computed and then discarded still printed its log line. A visual capture taken from that path can
therefore show a frame the user never saw, and an **unstyled** frame is exactly what an early or
discarded render of a not-yet-styled document would look like.

That is consistent with everything now measured: the cascade is right, the solver is right, the box
tree for the document is right, and the browser path and the offline path agree item for item. If
the capture is of a discarded frame, then the symptom is a **reporting and capture** bug rather
than a rendering bug - and this project has already found and fixed the reporting half of exactly
that.

**So before instrumenting anything else, re-take the capture from a frame that is known to have
committed**, and compare. This is cheap and it is upstream of every other hypothesis. If the
re-capture is correct, this entry closes as a capture bug, which would be the third distinct kind
of error found in this investigation - a wrong mechanism, then a broken counter, then possibly a
broken capture.

### A method note that applies to all of the above

`commit_render` returned at its identity gate before consuming `page.render_dirty`, and the
per-frame log ran before that gate. **Every frame log line read while investigating this symptom
may have belonged to a frame that was never committed.** Any measurement taken from those logs
describes a frame the user never saw. Confirm a frame committed before measuring it.

## S27 - Closed: phase budgets, and my premise about the hang was wrong

Was: `HttpTransport::with_proxy` set **`timeout_recv_response(None)` and
`timeout_recv_body(None)`**, so I recorded - from a browser-side observation, not from reading
the transport - that a transfer stalling after the connection was established **hangs forever**.

**That was wrong, and the way it was wrong is the useful part.** The response phase was
*already bounded*: ureq carries a send-side budget that covers the header wait, and a silent
server was measured at **415 ms against a 400 ms budget** on the direct path and 418 ms on the
proxy path, both before anything was changed. I inferred a permanent hang from a single 80-second
observation and never checked whether that phase was already covered by a budget under a
different name.

**The real defect is subtler than "hangs forever", and it was still real:**

- The response phase had **no budget of its own**. It borrowed ureq's send budget, which is
  unnameable and untunable without also moving the request write.
- The body phase had **no socket deadline at all** - the pump thread and its socket were left
  held indefinitely.

So the phase was bounded but not *controllable*, and the second was genuinely unbounded. Both
are now explicit fields.

| phase | field | default | failure mode it protects against |
| --- | --- | --- | --- |
| connect + TLS | `connect_timeout` (existing) | 5s | one black-holed address eating the whole request. `TcpConnector` gives each resolved address a slice of a geometric series, so a filtered AAAA record burned ~2/3 of a 30s budget with nothing reported |
| response status + headers | `response_timeout` (new) | `None` -> follows `timeout` (30s) | an origin or proxy that accepts the connection, swallows the request, and says nothing |
| body | `body_idle_timeout` (new) | `None` -> follows `timeout` (30s) | a connection that delivered part of a body and went quiet - half-open socket, dropped upstream |

**`None` means "follow `timeout`"**, which preserves the existing "one number bounds everything"
contract that 69 tests already depend on while making each phase independently settable. That is
why **no existing test needed a bigger budget and none was modified** - the shape was designed
around not disturbing them, and the agent says so explicitly.

### The body bound is an idle-read bound, and the mechanism is knowable

`CallTimings::next_timeout` derives `RecvBody`'s deadline from *now* on every read - idle - but
derives `RecvResponse`'s from when the headers completed, i.e. total. So `timeout_recv_response`
deliberately stays `None` and the response budget is carried on the send side, which is what
actually bounds the header wait, while `timeout_recv_body` is now set - the one body budget safe
to push to the socket. A total body budget would break large stylesheet downloads, which is
exactly what this engine struggles with.

### A timeout ends as a named terminal outcome, and does not return a poisoned socket to the pool

`"response headers: request timed out after 602ms"` and
`"body transfer: request timed out after 601ms"`, via `FetchError::Failed { phase, elapsed,
source: Timeout }` on the timeout path specifically, not only on the error path.

The pooled-connection evidence is two-sided: `a_body_stall_does_not_return_the_stalled_connection_to_the_pool`
asserts the stall **ran on the pooled socket** (the server's accept count is still 1) and that
the next request forced a **second** accept. And `a_body_timeout_releases_the_socket_instead_of_leaving_it_held`
was verified **load-bearing by reverting the timeout to `None`**, where it fails - the socket is
left held by an orphaned pump, exactly as the old comment documented. That is mutation testing
on the agent's own test, which is the standard I have been asking for.

The non-proxy path had the **same structural hole** but was already bounded, because
`HttpTransport::new` and `with_proxy` funnel through one construction site - so the only risk was
future drift. `the_proxy_and_direct_paths_enforce_the_same_phase_budgets` now pins same stall,
same phase, same failure kind, same wait, with and without a proxy.

### The payoff: live requests work, and it was not tuned to make them work

Through the system proxy: `example.com` 200/559 B in 87-200 ms, `iana.org` 200/6253 B, and
**`rfc9110.txt` 200/502,941 bytes in 462 ms cold and 356 ms pooled** - with a deliberately tight
200 ms body idle bound and a 2 s response bound.

**The 500 ms run outlived its 500 ms response budget and still completed**, which is the live
proof that the two are not the same knob. Nothing was tuned to make a request succeed: at a 300 ms
response budget the same 500 KB fetch fails, correctly and informatively, as `response headers:
request timed out after 388ms` - a real cold-connection cost through this proxy, now a
**diagnosis rather than a silence**.

This is what unblocks real-page measurement generally. Until now every reproduction on this
project had to be captured with `curl` and served locally, because the engine could not be
pointed at a live origin and get an answer.

### One honest mislabel, left in place deliberately

A **lazy TLS handshake** over a dead tunnel reports `ResponseHeaders`, not `TlsHandshake`, because
ureq handshakes on first write, under the send budget. It is bounded and named, but the label
follows the budget that covered it. Asserted as-is in a test and documented on
`FetchPhase::TlsHandshake` rather than papered over.

### The observation that started this, kept because the correction is the point

The browser's own request to a large Chinese portal through the working system proxy sat at TCP
**Established with zero progress for 80 seconds** with no stall report, while `curl` on the same
path returned 200 / 193164 bytes in 70 ms. That is what made this look like a permanent hang, and
it is what made real-page measurement impossible - every reproduction on this project had to be
captured with `curl` and served locally.

It was a real obstacle and the underlying defects were real. **But the "hangs forever" reading was
an inference from one observation, not a measurement**, and the phase was in fact already bounded
under a budget I had not looked for. The lesson is the one this register keeps re-learning: a
symptom observed once, from outside the component that owns it, is a hypothesis. The fix that
mattered came from the agent measuring the very phase it had been told about, and finding the
report it had been given wrong in its mechanism while right about its consequence.

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
