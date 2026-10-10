# Offline Real-Site Acceptance

This document exists to answer one question: **does a real-world page shape
render correctly, offline and repeatably?** Before this harness existed the only
answer was to open a window and look, which is why real-world regressions kept
shipping.

There are two halves, and they are deliberately separate:

| | Path | What it checks | Needs a build? |
| --- | --- | --- | --- |
| Static gate | `tests/test_real_site_capabilities.py` | the fixture's own markup and declared resources | no |
| Engine gate | `tests/real_site_tasks/` | parse, style, layout, paint, and resource discovery for the same fixtures | yes |

Site names in this document are test labels. Nothing here, and nothing in
either half, decides behaviour based on which site a URL belongs to. The engine
satisfies these fixtures through its ordinary HTML, CSS, layout, paint, and
resource paths, which is the law in `docs/generic-browser-todo.md` and what
`tools/check_site_neutrality.py` enforces mechanically.

## Commands

```powershell
# 1. the static gate (no build, no network)
pytest -q tests/test_real_site_capabilities.py

# 2. the engine gate (offline, deterministic, no network)
cargo test --manifest-path tests/real_site_tasks/Cargo.toml --test real_site_capabilities

# 3. the report-only render-and-diff
cargo test --manifest-path tests/real_site_tasks/Cargo.toml --test render_diff -- --nocapture

# 4. the opt-in HTTP smoke; the only thing here that touches the network
cargo test --manifest-path tests/real_site_tasks/Cargo.toml --test live_http_smoke -- --ignored --nocapture

# 5. render one fixture to a PNG for a human to look at
cargo run --manifest-path tests/real_site_tasks/Cargo.toml --bin real-site-shots -- --label netease_163_home --out target/shots/netease_163_home.png
```

`tests/real_site_tasks` is a standalone crate outside the engine workspace, and
it is invoked with `--manifest-path` on purpose: adding it as a workspace member
would put it in the root `Cargo.lock`, which is contended whenever several
crates are changing at once. The root `.gitignore` already excludes
`tests/real_site_tasks/Cargo.lock`.

One consequence worth knowing: because this crate is its own workspace root,
cargo's default target directory is `tests/real_site_tasks/target/`, so the
dependency graph is compiled a second time here rather than reused from the
engine workspace's build. Sharing the repository `target/` would need a
`.cargo/config.toml` carrying `build.target-dir = "../../target"`; that is
deliberately not done, because it would also queue this crate's build behind the
engine's. Nothing sets `CARGO_TARGET_DIR`. The nested `target/` directory is
already covered by the root `.gitignore`'s unanchored `target/` rule.

## Captures: facts measured from live pages

The shape fixtures above are held to the shape contract, which asks every page
for landmarks, declared scroll geometry, and similar. A live page does not owe
the project any of that, so real captures live in a separate tier:
`tests/real_site_tasks/src/captures.rs` and `tests/real_site_captures.rs`.

Each capture is a reduced snapshot of a live page, in
`tests/fixtures/real_sites/<label>.html` with its stylesheet in `<label>.css`.
Its expectations are measured from the **raw** bytes with Python's
`html.parser` (title, `a[href]`, named controls, submit inputs, image
resources, stylesheet and script links, story rows), and the reduced fixture
is checked to reproduce those numbers before it is committed. The engine is
then held to what the page says: the title, the counts it discovers, and the
boxes it lays out for the parts that matter. A failure is therefore a
disagreement with the page, not a broken shape rule.

Two rules that the captures already needed:

- **A declaration the server sends in a header must be in the markup.** The
  harness decodes fixture bytes without HTTP headers, and the sniffing
  algorithm reads a `<meta charset>` only within the first 1024 bytes. The
  Hacker News capture carries `<meta charset="utf-8">` at the top of the file,
  because the server sends `charset=utf-8`.
- **Count what the page means, not what the markup could match.** A search
  form's submit buttons appear twice on Google's page, once visible in the
  form and once in a hidden autocomplete popup, so the contract checks the
  two buttons a user sees, by their labels.

- **A scripting-enabled browser's view of the markup is the reference.**
  `noscript` content is text to such a browser, so an `img` inside it is not a
  resource a load fetches, and a `script[nomodule]` is a fallback it does not
  run. Each capture records these exclusions as their own count, so the
  expectation is what the page does, and the exclusion is visible.
- **A JavaScript-rendered shell has no static text to lay out.** The YouTube
  capture's search field is `hidden` in the static markup, and its skeleton
  carries no text. Its capture is flagged `js_rendered_shell`, so the layout
  checks skip it, and its discovery facts still hold.

Run them with `cargo test --manifest-path tests/real_site_tasks/Cargo.toml --test real_site_captures`.

## The fixtures

`tests/fixtures/real_sites/` holds five reduced page shapes. Each is one
`<name>.html` plus one `<name>.css`; the CSS is the deterministic local response
the harness serves in place of the fixture's external stylesheet.

| Fixture | Page it reduces | Scroll region | Markup |
| --- | --- | --- | --- |
| `baidu_home` | the Baidu home page | a 20-item ranked hot list | 102 lines |
| `baidu_results` | the Baidu search-results page | 12 result articles plus a pager | 138 lines |
| `zhihu_home` | the Zhihu home feed | a 20-item hot topic list | 125 lines |
| `zhihu_article` | a Zhihu `zhuanlan` article | five comments under four article sections | 94 lines |
| `netease_163_home` | the NetEase portal home page | a 24-entry ranking, plus seven channel sections | 189 lines |

They are **reduced snapshots of common page shapes**, not captures and not
alternate runtime implementations. Real-site URL shapes are kept so that URL
resolution, stylesheet discovery, image discovery, and script discovery are all
exercised exactly as a live load would exercise them. The responses for those
URLs are replaced with deterministic local values, so **no check in either half
requires Internet access**.

A reviewer should be able to read one fixture and understand the layout it
asserts. That is why the layout rules live in a small adjacent stylesheet rather
than in a hundred lines of inline `style` attributes, and why every fixture stays
under 200 lines of markup.

One deliberate reduction: each fixture's side column is **stacked under** the
main column instead of floated beside it. The ordering assertion in item 7 is
only meaningful while the two columns do not interleave, so no fixture in this
set uses a float or a negative offset to fake a side column. A two-column fixture
would need that assertion scoped to one column first.

### What the portal fixture keeps that the others do not

`netease_163_home` deliberately preserves the resource shapes observed on the
live portal, because those are the interesting ones:

- CSS and scripts from `static.ws.126.net`, referenced by **two** separate
  `<link rel=stylesheet>` elements, so DOM-order cascade across two external
  responses is part of the contract;
- images from `nimg.ws.126.net`;
- `img.lazy` elements carrying `data-src` / `data-original` and no `src` yet -
  exactly how the portal defers below-the-fold art;
- two `srcset` candidate lists, one with `w` descriptors plus `sizes` and one
  with `x` descriptors;
- a `<video poster>` and an inline `<svg>` filing badge.

## The contract

The two halves assert the same seven-item contract, at different depths. The
Python gate reads only the fixture bytes and the fixture's own stylesheet, so it
can check the markup-level half of items 1 to 5 and the *declared* geometry
behind item 6. The Rust gate loads each fixture through the engine, so it checks
all seven against the DOM, the computed styles, the fragment tree, the display
list, the resource plans, and the raster. Which half owns which assertion:

| Item | Python gate | Rust gate |
| --- | --- | --- |
| 1. decoded title | charset reachable, title text, no `U+FFFD` | the same, through `decode_html_bytes` |
| 2. landmarks | tags present, one `main` | present **and** each laid out as a non-empty box |
| 3. usable links | resolve, named | resolve, named, **and** laid out with non-zero area |
| 4. search landmark | role, named input, submit control | the same, **and** the input is laid out |
| 5. resource classification | `rel` / `type` / attribute classification | the discovery plans, **and** the sheet reached layout |
| 6. layout past 600px | declared item count and declared item heights | measured scroll extent and fragment count |
| 7. ordered scroll blocks | N distinct non-empty blocks; ascending ordinals where the feed has them | strictly increasing laid-out `y`, **and** whole-page reading order |

1. **A decoded document title.** The raw fixture bytes go through the HTML
   encoding-sniffing algorithm (`render_core::html::decode_html_bytes`, and the
   equivalent BOM/meta-charset reader in the Python gate). The title must match
   exactly, must contain non-ASCII text so the decoding is observable, and must
   contain no `U+FFFD` replacement character. Both halves also require the
   `<meta charset>` declaration to be reachable inside the first 1024 bytes: past
   that boundary the engine correctly falls back to windows-1252, which is a
   fixture bug rather than a capability to assert.
2. **Landmark semantics.** `header`, `nav`, `main`, `article`, `section`, and
   `aside` are all present, exactly one `main`, and every landmark is laid out
   as a non-empty box.
3. **Usable links.** Every `a[href]` has a non-empty `href` that resolves against
   the document base URL to an `http`/`https` URL, has an accessible name (its
   own text, or the `alt` of an image inside it), and is laid out with non-zero
   area. "Laid out" is deliberately generous, because the layout attributes
   things differently: a block owns a box, an inline-block owns a box, a plain
   inline owns neither, and its text lines belong to the text nodes under it.
4. **A `role=search` form.** Exactly one form carries `role=search`; it holds a
   named, non-hidden `<input>` and a submit control; the input is laid out with
   non-zero area.
5. **Resource classification.** The count of eligible external `<link
   rel=stylesheet>` slots, the count of elements that declare a fetchable image
   source, and the count of discovered scripts all match the fixture's declared
   numbers, and image discovery finds exactly the elements that declare a
   source - no more, no fewer. Every discovered image URL is a fetchable
   absolute URL, and every discovered script is a deferred external script.
   The external stylesheet must also be *applied*, not merely discovered: after
   layout the `<main>` content column is exactly as wide, and starts exactly as
   far right, as the fixture's stylesheet declares. No UA rule declares those
   numbers, so a sheet that was fetched but dropped fails here.
6. **Block layout that continues below a 600px viewport.** The contract viewport
   is 1280x600. The document must scroll, its scrollable content must be taller
   than 600px, something must be laid out below the fold, and the reference
   layout must produce more than a hundred box fragments. The Python gate proves
   the precondition from the fixture's own declarations: enough block items,
   each with a declared pixel height, that they cannot fit in 600px.
7. **Ordered text blocks in the scroll region.** The scroll region is found
   structurally: inside `main`, the element with the most direct element
   children that each own laid-out text. Those blocks must each carry text, must
   appear in strictly increasing document-space `y` order, and the last one must
   end below 600px. Separately, sorting every text line in the page top-to-bottom
   must not move any line ahead of a line that precedes it in the document.

Every fixture also has to carry alt text on every image, resolvable
`data-src` / `data-original` candidates on every deferred image, `srcset` images
resolving to a real candidate from their own list, `video[poster]` images
discovered as posters, and enough `section` elements carrying three or more links
to count as channel sections. Those counts come from each fixture's own table
entry and are zero wherever the shape does not occur - which is why only the
portal fixture has non-zero ones, while still being checked by the same code.

## Image discovery and deferred images

The engine's image discovery plan runs for every fixture and the engine must
report **no** discovery error: an unresolvable URL, an unsupported scheme, a
limit, or a `srcset` that resolved to nothing all fail the test.

Exactly one diagnostic is permitted: `MissingSource`, reserved for an image
whose bytes have not been requested yet. It is permitted rather than required
because it is currently declared but never constructed - a `data-src`-deferred
image simply produces no discovered resource and no diagnostic at all. The test
asserts the direction that matters: a deferred image is never fetched, and
anything reported as source-less is one of the deferred images.

`srcset` selection **is** implemented (`crates/render-core/src/image.rs`),
including `sizes` and both the `w` and `x` descriptors, so a `srcset` image
resolves to a real candidate rather than an unsupported diagnostic.
`SrcsetUnsupported` is a declared-but-never-constructed dead variant; no test
expects it, and `MissingSource` is in the same state today.

## Determinism

The render uses the engine's reference backends - `SimpleTextMeasurer`,
`ReferenceTextShaper`, `NoGlyphMasks` - never system fonts. The text measurer is
the same one `crates/render-layout`'s solver tests and
`crates/render-core`'s conformance tests use, and it is the shape
`render-perf` is built around. That is what makes a pixel comparison meaningful
across machines, and it is asserted rather than assumed: one test loads every
fixture twice and requires the two fragment trees and the two rasters to be
equal.

Deterministic image responses are generated from the requesting element's own
declared `width`/`height` and from an FNV-1a digest of the requested URL. Nothing
in that path looks at which site a URL belongs to.

## Render-and-diff: report-only by default

`tests/render_diff.rs` renders every fixture headlessly, compares the raster
against a checked-in baseline **when one exists**, and prints the result:

```text
render-and-diff report (viewport 1280x600, reference backends, no system fonts)
report-only: a pixel difference is printed, never asserted

baidu_home         no-baseline              0 / 768000 px differ (0.0000%), max channel delta 0
    raster 1280x600, digest 0c25fd35, 157 display items, 321 fragments, max scroll y 1805.2
netease_163_home   no-baseline              0 / 768000 px differ (0.0000%), max channel delta 0
    raster 1280x600, digest e117d375, 300 display items, 619 fragments, max scroll y 4172
```

It never fails on a pixel difference, and it never creates a baseline. Both are
deliberate:

- `docs/visual_fidelity_gaps.md` records that today's renderer is wrong in
  several known ways. A baseline generated from current output would encode that
  wrongness as truth, and would then make every honest fix look like a
  regression.
- A pixel gate that fails while those gaps are open trains everyone to ignore
  it, which is worse than having no gate at all.

**No baseline is checked in today**, so every row reads `no-baseline` and the
test passes unconditionally. A unit test pins the comparison arithmetic itself
(PNG round-trip, pixel counting, shape-mismatch reporting), so the machinery is
proven even with no baseline to compare against.

### What a review shot does and does not show

Read this before judging a PNG. The reference path uses `NoGlyphMasks`, which
returns no outline for any glyph, so **the reference raster paints no text at
all**. A review shot shows:

- box geometry, background and border colours, and images;
- link underlines, because `text-decoration` reached paint and is drawn as a
  real display-list item;
- **not** glyph shapes, and therefore not letterforms, not where a line breaks
  inside a word, and not the shape of a rendered run.

That is the price of a raster that is byte-identical across machines: it cannot
depend on system fonts, and with no font there is nothing to draw. It also means
a pixel baseline pins *layout*, not typography - one more reason not to generate
one automatically. Judging typography needs a build that loads a real font, and
that is deliberately not something this harness does.

### How a human reviews and promotes a baseline

Promotion is a human decision, and it is deliberately not something running the
tests can do:

```powershell
# 1. render a review shot
cargo run --manifest-path tests/real_site_tasks/Cargo.toml --bin real-site-shots -- --label netease_163_home --out target/shots/netease_163_home.png

# 2. open target/shots/netease_163_home.png and judge it.
#    Ask, against docs/visual_fidelity_gaps.md: is anything wrong here a known gap?
#    If yes, do not promote. Close the gap first.

# 3. only once the image is right, promote it
cargo run --manifest-path tests/real_site_tasks/Cargo.toml --bin real-site-shots -- --label netease_163_home --out target/shots/netease_163_home.png --promote

# 4. confirm the report now compares against it
cargo test --manifest-path tests/real_site_tasks/Cargo.toml --test render_diff -- --nocapture
```

`--promote` is refused unless `--out` names a review shot that already exists, so
the command that writes a baseline always names the image that was looked at. It
writes `tests/fixtures/real_sites/baselines/<label>.png` and nothing else. A
promoted baseline is a claim that this fixture renders correctly at the contract
viewport with the reference backends; if that claim stops being true, the
correct response is to look at the diff and decide which side is wrong, not to
re-promote.

Because a baseline pins the viewport, the baseline PNG's dimensions must match
the capture. A mismatch is reported as `shape-mismatch`, not as a difference.

## Gap markers

`tests/real_site_tasks/tests/real_site_capabilities.rs` ends with `#[ignore]`d
tests. Each names the entry in `docs/visual_fidelity_gaps.md` that makes it
impossible today, and each will start passing when that gap is closed - at which
point the `#[ignore]` is deleted and the test joins the gate. Per
`docs/generic-browser-todo.md`, a gap is implemented forward and never removed,
so these are to-do markers, not workarounds.

| Ignored test | Gap |
| --- | --- |
| `text_measurement_depends_on_the_requested_font_family` | S1 - `TextStyle` carries no family, weight, or style |
| `an_inline_svg_shape_is_laid_out_and_painted` | S5 - an inline SVG shape still lays out no box |
| `a_sticky_header_stays_in_the_scrollport` | S6 - `position: sticky` has no layout consumer |
| `the_capability_registry_declares_web_font_and_animation_support` | S4 / S9 - `@font-face` is discarded and untracked |
| `no_fixture_reports_any_parse_error` | not yet listed - see below |

### Markers that have already been retired

Two of these started life as markers and are now part of the gate, because the
capability landed while the fixtures were being built:

- `link_underlines_reach_the_display_list` was the S3 marker. `text-decoration`
  now reaches paint, so every `nav a[href]` in the Baidu fixture contributes a
  `DisplayCommand::TextDecoration` to the display list. The marker is gone and
  the assertion is permanent.
- `inline_svg_is_parsed_as_foreign_content_with_geometry` was the S5 marker. The
  foreign-content half landed - an inline `<svg>` and its `<path>` are in the SVG
  namespace - so that half is now a permanent test. The geometry half did not
  land, so it was split out into
  `an_inline_svg_shape_is_laid_out_and_painted`, which is what remains ignored.

### An engine defect this harness found

`no_fixture_reports_any_parse_error` is the one marker that does not name an
entry in `docs/visual_fidelity_gaps.md`, because the gap list does not have it
yet. Building these fixtures surfaced it:

```text
crates/render-html/src/tokenizer.rs:477-483
```

In the start-tag attribute loop, the `MissingWhitespaceBetweenAttributes`
diagnostic fires when the attribute that *follows* a valueless attribute is
reached, even though whitespace separates the two:

| Markup | Reported |
| --- | --- |
| `<script defer src="a.js">` | `MissingWhitespaceBetweenAttributes` |
| `<script src="a.js" defer>` | clean |
| `<div a b>` | `MissingWhitespaceBetweenAttributes` |
| `<div a="1" b="2">` | clean |

`had_whitespace` is not carried across `consume_attribute` for an attribute with
no value, so the flag reads false on the next iteration. `<script defer src>` is
close to universal on the real web, so this is not a corner case.

The fixtures deliberately keep the natural attribute order rather than
reordering to dodge it, and the passing test
`the_only_parse_error_reported_is_the_documented_tokenizer_defect` asserts that
`MissingWhitespaceBetweenAttributes` is the *only* diagnostic any fixture
produces. Any other parse error still fails immediately.

### Gap-adjacent behaviour that is asserted as passing

Two things the engine does the honest thing about are asserted as **passing**
today rather than parked: a document without a doctype reports
`QuirksModeUnsupported` (S7) instead of silently applying standards-mode box
model rules, and a deferred `data-src` image is never fetched and never reported
as an error.

## What this harness does not claim

This is an offline compatibility gate over reduced page shapes. It does not claim
that login, personalized feeds, anti-bot flows, or any particular JavaScript
interaction on the live sites is implemented. It does not execute page scripts;
it asserts that they are discovered, classified as deferred external scripts,
and resolved to absolute URLs. It does not fetch any of these sites.

The one networked check is `tests/live_http_smoke.rs`, which is `#[ignore]`d so
that a default run stays offline. It uses only `render-net` and asks a single
question: can the transport still reach the modelled origins and get a page back?
It covers the three origins the fixtures model - `https://www.baidu.com/`,
`https://www.zhihu.com/`, `https://www.163.com/` - plus the Baidu search
endpoint. It deliberately does not use article or channel URLs: a content id
that no longer exists answers 403, which says nothing about the transport and
would only make the test rot.

A DNS failure, timeout, TLS failure, or other network-availability error is
reported as a skip, so an unavailable network does not become an ordinary test
failure. A reachable endpoint that answers non-2xx, with an empty body, or with
a non-HTML content type does fail.
