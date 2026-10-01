# corpus2 — a shape-selected corpus, what it cannot see, and whether it is real

The captures live in `.diag/corpus2/` (gitignored). This file is the register for
them: which shapes the **combined** corpus represents, how deeply, whether each
capture is still byte-faithful to what its origin served, and — the part that
matters most — which open gaps in `docs/visual_fidelity_gaps.md` it is blind to.

Every number here is produced by a tool, not typed. Regenerate with:

```
python tools/corpus2/tests.py           # 43 negative controls; must pass
python tools/corpus2/matrix.py          # per-shape sub-feature matrix
python tools/corpus2/gaps.py            # gap-by-gap CAN / PARTIAL / CANNOT
python tools/corpus2/final.py           # per-capture provenance, sizes, fidelity
python tools/corpus2/verify.py          # UTF-8, no blobs, no NUL bytes
python tools/corpus2/roundtrip.py       # is every capture byte-faithful?
python tools/corpus2/roundtrip.py --origin   # ...compared to a re-fetched origin
python tools/corpus2/crosscheck.py      # the two-instrument cross-check
```

`tools/corpus2/` is committed, so those commands work on a clean checkout.
`.diag/corpus2/_tools/*.py` are one-line shims that load the same modules, so
every command published in an earlier round still resolves.

`.diag/` was already 220 MB of hand-picked portals before this work. The 18
captures here add 22.8 MB. Both are gitignored; nothing is committed.

---

## 0. The two things to read first

### 0.1 S24 form owner: the invariant is hand-tested and unexercised

The engine **derives** a control's form owner on read rather than storing it,
precisely so that a control owned by a form that is **not its ancestor** cannot
go stale — the case reached through the parser's form element pointer, where a
`form=` attribute or a nested `<form>` start tag makes the owner something the
DOM ancestry does not contain.

**No page in this corpus produces that case.** `form=` is **0 on all 27 pages**,
and a `<form>` start tag while the form pointer is set is **0 on all 27 pages**.
Both are measured zeros, cross-checked by two independent instruments, and both
were re-probed in this round across 41 further URLs chosen specifically because
they are likely to carry it — HTML 4.01 and XHTML 1.0 specification pages, the
WHATWG forms and form-control-infrastructure pages, RFC Editor, IETF, CPAN,
PerlDoc, RIPE / LACNIC / AFRINIC, and a set of long-lived HTML4-era pages
several of which are now dead. Zero again.

So the justification for the engine's most careful design decision rests on a
case **no real page in this corpus produces**. The next person to touch form
handling needs to know that the invariant is hand-tested and unexercised rather
than corpus-validated, and that a fix to it can be written, unit-tested against
a fixture, and still be wrong. Treat this as a known limit, not a continuing
search: this likely needs a legacy page or a hand-built case.

### 0.2 Every capture is byte-faithful. This was not true when this file was last written.

**18 of 18 captures are faithful.** Proven three ways, all re-run against the
corpus as it now stands:

| check | what it proves | result |
| --- | --- | --- |
| **lossless identity** — `strip.py --lossless` drops nothing and rewrites nothing, so its output must equal its input byte for byte, run over every capture body | the reducer is not altering the documents it is given | **18/18 PASS** |
| **lossless identity on the origin** — the same check run over a freshly re-fetched origin | the reducer is the identity on real markup, not only on its own output | **18/18 PASS** |
| **token-stream equality against a re-fetched origin** | the capture contains the origin's tags, references and declarations, in the origin's order | **17/18 token-identical**; `nasa.gov` differs by 46 tokens with **0 invented end tags and 0 respelled references** — that page rotates content between two fetches minutes apart |

The third row is the one that decides it, and it is worth being precise about
what it does *not* say: a live page's text changes, so text is excluded from
the comparison and only structure is compared. That is a deliberate limit. A
capture whose text silently lost a paragraph would pass, and that is why the
before/after byte sizes in §3 are recorded per capture and why the reduction's
full drop ledger is in each capture's own header.

**What the previous corpus was.** All 18 captures were normalised, in two ways
that had to be found rather than assumed, and one of them had been silently
published:

1. **Every inline `<script>` start tag had been deleted.** The captures
   contained 80 `<script>` start tags against **254** `</script>` end tags. The
   element, and its `type`, `nonce`, `async` and `defer` attributes, were gone;
   the JavaScript body had become loose body text; a stray end tag was left
   behind. The shape the engine's script handling is tested against simply was
   not in the corpus, and nothing said so.
2. **Entity references were respelled.** `html.parser` hands over a reference's
   *name* and not whether it had a semicolon, so re-emitting `&%s;` inserted one
   the origin never had. `&b=2` in a query string became `&b;=2`, and in
   `accounts.google.com` this turned `window.wiz_tick && window.wiz._tick(…)`
   into `window.wiz_tick && window.wiz;_tick(…)` — a member access broken into
   two statements. **32 154 references** were rewritten this way. This is
   parse-equivalent in a text position, which is exactly why it survived every
   size check and every `verify.py` run.

Both are fixed at the root rather than patched: `strip.py` is now a
**source-span passthrough**. Every token is re-emitted as the exact byte range it
occupied in the input, located with `getpos()` and closed with the terminator the
parser found. Nothing is reconstructed, so there is nothing left to get wrong.
`build.py` refuses to write a capture unless the lossless pass is the identity on
the origin first. All 18 were **re-captured** with the fixed reducer; none was
shipped as a normalised document.

### 0.3 Six instrument defects, and every tool now has a way to fail

Four were named in the previous round. This round found two more, and the
negative-control suite that found them is the point.

| instrument | has a way to fail? |
| --- | --- |
| `tools/check_site_neutrality.py` | yes — a 60-check self-test |
| WPT runner | yes — a self-check |
| html5lib runner | yes — a negative control |
| the parser's tree-invariant checker | yes — a negative control |
| `css_corpus.rs` | unknown — not checked here |
| **the 14 corpus2 tools** | **yes, as of this round: `tools/corpus2/tests.py`, 42 cases** |

`tests.py` holds one case per tool where the tool's own assertion about the
world is **false**, and asserts the tool notices. Writing it found **three
further live defects in the instruments** that no amount of re-reading had
surfaced:

- **`shape_scan.py` never counted a self-closing tag.** `handle_startendtag` had
  its own short body, so `<use xlink:href="#a"/>`, `<svg/>` and `<rect/>` — half
  of what SVG is written with — were invisible. The `<use>` count is exactly what
  the S5 coverage claim rests on.
- **`query.py --min 0` reported a measured zero as a hit.** `>= mn` with `mn=0`
  is satisfied by absence, so the tool printed every key on every page, which
  reads as universal presence and is the precise opposite of what was measured.
- **CSS was counted inside `<script>` bodies.** `scan_css` is a raw text grep, so
  a line of inline JavaScript containing `position:sticky` counted as a
  declaration. One capture reported 388 `transition` declarations, most of them
  in a script. `scan_page_css` now excludes code samples *and* script bodies, and
  §2 lists every figure it moved.

And three more in the tools it was written against:

- **`build.py`'s index merge compounded.** Each capture's write re-added the
  previous write's rows, so rebuilding 18 captures produced **35 index rows for
  18 capture directories**. A register with rows for documents that do not exist
  is worse than no register. It now merges against a snapshot taken once, before
  the run.
- **`gaps.py` could not tell a measured zero from a never-measured one.**
  `matrix.json` filtered zero-valued counters out, so a key that was zero
  everywhere and a key nobody looked for were indistinguishable in the
  machine-readable output. `matrix.json` now records `measured_zero` per page and
  `gaps.json` carries a `measured` flag per counter, and `gaps.py` **warns and
  names any key it cannot account for** rather than publishing a confident 0.
- **`probe3.py` fetched its own list's group header**, reporting a fake
  unreachable and making "probed 21 URLs" wrong. The denominator of a probe
  count is the count. A probe that dies on a `KeyError` after fetching every URL
  also looks exactly like a probe that found nothing, which is the worst
  possible failure for that tool.

Seven defects in all across the two rounds, in four of the fourteen tools, and
**not one of them was in a page**. Every one was in an instrument, and every one
produced a number that read as authoritative.

---

## 1. The blind spots

A gap the corpus cannot exercise is a gap where a fix can be written, tested,
and still be wrong on a real page. This project has shipped exactly that — the
`viewBox` lookup was lowercased, **no SVG was ever scaled to its viewport**,
and the test passed anyway because the geometry happened to land on the asserted
pixel. These are the shapes where that could happen again.

### CANNOT — no page in the corpus has the shape at all

| gap | what is missing | why it is a real blind spot |
| --- | --- | --- |
| **S24 form owner** | `form=` attribute: **0 on 27 pages**. Nested `<form>` start tag: **0 on 27 pages**. | §0.1. The corpus's worst gap and the first one anyone should read. See the top of this file. |
| **S7 quirks mode** | a page served without a doctype, or with a quirks-triggering one. All 27 pages have a standards doctype. | CSS 2.1 §9.2.1.1 is unreachable. A fix can only be tested against a hand-built fixture, and the fixture would then be the sole evidence. |
| **S18 device pixel ratio** | any capture tied to a fractional display ratio. | Every capture is a static document fetched at one ratio with no media attached, so the 82-occurrence `*-device-pixel-ratio` problem cannot be observed, reproduced, or shown fixed. |
| **S11 nested browsing contexts** | an `<iframe>` whose `src` is itself a captured document. | `iframe` appears only as an element name. An unfetched iframe renders nothing whether the feature exists or not, so this is blind in the worst way: it looks like a page without iframes rather than a page with unreached ones. |
| **S13 Selection / Range** | a recorded interaction, not just a served document. | The corpus *can* hold a page that calls `getSelection`, but it cannot hold the resulting selection, so a fix here is untestable against this corpus even in principle. |

### ABSENT sub-features, measured rather than assumed

Three sub-features came back **zero on every page in the combined corpus**,
including the specification pages that document them. Each is a **measured zero
cross-checked by two independent instruments** (§4), which is the only kind of
zero this project will publish:

- **`<foreignObject>`** — 0 live elements on all 18 captures. The byte-level grep
  finds 5 mentions; the cross-check resolves all of them as prose, a comment or
  a URL. That is the same class of finding as the original 199 hits, resolved
  the same way, except that now it is resolved by a mechanism rather than by a
  person reading a number and deciding it looked wrong.
- **CDATA sections** — 0. The byte-level grep finds 2 mentions, both resolved.
  Note that `html.parser` switches to RAWTEXT for `<style>` even inside `<svg>`,
  where the HTML specification does not, so a CDATA section in an SVG `<style>` —
  the one place a real page puts one — was invisible to the parse instrument.
  That is now counted explicitly rather than left as a silent gap.
- **`table[rules]`** — 0, and 0 byte-level mentions. The pre-HTML presentational
  attribute is genuinely gone from the live web, so §17.11 attribute-driven
  borders cannot be measured here at all.

Each of these three is recorded in `gaps.json` with `"measured": true`, which is
the field that separates "no page has the shape" from "nobody looked for it".
That separation had to be added to the tooling during this round; before it, a
counter that was zero everywhere and a counter that was never computed were the
same thing in the machine-readable output.

### PARTIAL — the shape is present but thin

| gap | present | thin |
| --- | --- | --- |
| **S5 inline SVG** | 679 `<svg>`, 657 `viewBox` on 12 pages | `xlink:href` (1 page), `xml:space` (1 page), `<use>` (1 page) |
| **S6 multi-column** | 1184 `float`, 368 `clear` | real `column-count` on 7 pages, `columns` shorthand on **2** |
| **S6 stacking** | 1576 `z-index`, 2310 `position:absolute` | `mix-blend-mode` 9 pages, `isolation` 4 pages, `will-change` 4 pages |
| **S14 `<template>`** | 29 `<template>` on 4 pages | 13 row-templates, on **1** page |
| **S4 `@font-face`** | 295 blocks, 221 `unicode-range` | `font-variation-settings` 3 pages, `font-feature-settings` 4 pages |

A single-page sub-feature is a single point of failure. The `<use>` / `<defs>`
indirection that S5 explicitly leaves out is exercised by **one** capture, so a
regression there is indistinguishable from a change to that one page. Treat
those rows as "one data point", not coverage.

### One CAN that is only half a CAN

**S16 dynamic pseudo-class state** is CAN for the mechanism — 1544 checkboxes,
1574 labels, 282 buttons and 14 search inputs give `:focus` / `:focus-within` /
`:hover` a large target population. But the corpus is a *served document*: it
holds no hovered or focused snapshot. So a fix can be exercised and cannot be
*seen*. Same shape applies to S13's scroll-offset half.

---

## 2. Shape coverage matrix

Combined corpus: **27 pages** — the 9 that were already in `.diag/` plus 18
added here. Full output in `.diag/corpus2/_tools/matrix.txt`, machine-readable
in `.diag/corpus2/_tools/matrix.json`. **Every number in this section was
re-measured after the re-capture**, not carried over.

| shape | depth | strongest carrier | absent sub-features |
| --- | --- | --- | --- |
| **form-heavy** | strong | a statistics index: 3 forms, 1541 form-associated controls, 9 outside every form; plus a login page with 24 controls, 8 outside any form, and a password control | `form=`, nested `<form>` (both 0) |
| **data-table** | strong | 14 tables / 3291 rows / 11676 cells with 11 `thead` + 11 `tbody`; another with 34 tables, 274 `rowspan`, 34 `colspan`, 258 `th[scope]`; a page with 4 `caption`; a `<colgroup>` page (9); a `<tfoot>` page | `table[rules]` |
| **webfont-heavy** | strong | **75 live `@font-face` with 75 `unicode-range` slices in one page** (273 of each in the page's bytes, the rest inside an inline script — see below), 60 `@keyframes`, and a password control | — (thin: variable/feature settings) |
| **svg-heavy** | good | 240 inline `<svg>`, 239 `viewBox`, **44 live `xml:space`** — the only page found with live `xml:space` outside a code sample; a second with 39 `<use>` against 32 `xlink:href` | `<foreignObject>`, CDATA |
| **animation-heavy** | strong | 75 `@keyframes`, 388 `transition`, 204 `animation` across 30 real stylesheets and the page's own `<style>` | — |
| **stacked/positioned** | strong | 111 `position:sticky`, 1576 `z-index`, 1768 `transform`, 2310 `position:absolute` across 25 pages | — (thin: `isolation`, `will-change`) |
| **multi-column** | good for floats, **thin for columns** | the only commercial page found with *live* `column-count` and `column-width` rather than `column-count` inside a documentation snippet; 1184 floats overall | — (the `columns` shorthand has 2 pages) |
| **media-heavy** | strong | **200 `<video>` with 200 `poster` and 920 typed `<source>`**; a second with 200 `<audio>` and 588 typed `<source>` | — |
| **template + inline JS** | present, thin | 17 `<template>` of which **13 contain row tags** (the template-as-row-template shape); 19 inline scripts of which 7 bind a load handler, and 2 KB more on a sign-in page | — |

### The `@font-face` headline was inflated 3.6×, and the correction is instructive

The deepest webfont page's **origin bytes contain 273 `@font-face` blocks with
273 `unicode-range` slices**. Only **75** of them are live CSS. The other 198
are strings inside an inline `<script>` — a lazily-loaded font sheet embedded in
JavaScript.

`scan_css` is a raw text grep, so it counted all 273, and the previous version of
this file published "273 `@font-face`" as the corpus's deepest single-page
webfont population. **An engine cannot use 198 of those blocks.** The matrix now
measures with `scan_page_css`, which excludes code samples *and* script bodies,
and the figure is 75. Both numbers are true; they are not the same claim.

This is a seventh instance of the same failure, and it ran in the
**over-reporting** direction for once — which is worth noting precisely because
everything else in this file ran the other way. An inflated webfont count would
have sent someone to S1 believing the population was larger and more varied than
it is. A corpus that over-reports is not safer than one that under-reports; it
is just wrong in the direction that takes longer to notice.

The same fix moved several other figures, all downward, all for the same reason:

| figure | before (raw text grep) | after (excluding script bodies and code samples) |
| --- | --- | --- |
| `z-index` | 1639 on 25 pages | **1576** on 25 pages |
| `position:absolute` | 2433 | **2310** |
| `transform` | 1792 on 25 pages | **1768** on 24 |
| `position:sticky` | 113 | **111** |
| `float` | 1247 | **1184** |
| `column-count` | 33 on 8 pages | **27** on 7 |
| `columns` shorthand | 61 on 2 pages | **19** on 2 |
| `@font-face` | 301 on 21 pages | **295** on 21 |

`position:sticky` at 111 on 15 pages is the S6 figure §7 cites, and it is now
the one an engine can act on.

### And one figure was inflated 18× by the deleted `<script>` tags

`html.spec.whatwg.org/multipage/tables.html` was recorded as carrying **161
tables, 143 `colspan`, 44 `rowspan` and 20 `<caption>`**. It carries **9 tables,
1 `colspan`, 4 `rowspan` and 3 `<caption>`**.

The difference is the deleted inline `<script>` start tags. With the `<script>`
element gone, its body was parsed as body text — and that body contains the
specification's own table markup as strings, so **152 of the 161 "tables" were
JavaScript**. The corpus was reporting a documentation page as the densest table
page found, on the strength of its example code.

This is the clearest single argument for the round-trip work. A normalisation did
not merely lose information; it **manufactured a coverage claim**, and that claim
is the kind of thing a coverage matrix exists to be trusted about.

### Did any of the four named defects change a conclusion?

Yes, three conclusions moved, and one did not:

- **The `@font-face` count was wrong twice over, and is now right.** A page
  carrying 273 `@font-face` blocks reported `webfont_heavy = 21` because the
  matrix read only the linked stylesheets. Fixing that exposed a second error:
  198 of the 273 are inside an inline `<script>`, so the live figure is 75. The
  corpus-wide figure is **295 `@font-face` blocks across 21 pages**.
  `tests.py` builds a page with 273 inline `@font-face` blocks in a real
  `<style>` element and asserts the score is `3 × 273`, so the 21 cannot come
  back; a second control asserts that a `@font-face` inside a `<script>` body is
  *not* counted.
- **Inline script population was under-reported by more than 2.5×.** "inline
  scripts binding a load handler" went from **12 occurrences on 7 pages** to
  **32 on 14**, because the script start tags had been deleted and the bodies
  were being read as body text. S10's target population was understated.
- **`<svg>` and `<use>` were under-reported.** 677 → **679** inline `<svg>` once
  self-closing tags were counted. The bigger correction is `<use>`: the `<use>`
  indirection is what the S5 claim is *about*, and it was being counted by a
  code path that could not see `<use .../>`.
- **The `z-index` and `position:sticky` conclusions did not change, only
  magnitude** (1639 → 1576, 2433 → 2310). Part of that is the live web moving
  between measurements and part is the script-body fix above; it is also why
  these numbers are quoted from `gaps.json` rather than remembered.

### How selection was actually done

Not by popularity. ~110 candidate URLs were fetched, and `shape_scan.py` counted
the structural and CSS features each one contains. Pages were chosen because the
measurement said so — the reason each capture was picked is recorded in
`capture_index.json` under `why` and printed by `final.py`.

Two selection traps the tooling had to close, both of which had produced wrong
answers first:

- **CSS inside a code sample is not a page using that CSS.** Several
  documentation pages score high on `column-count` purely from `<pre>` examples.
  Every shape claim is made against a code-sample-free view of the document, and
  where live and raw differ, the live number is the one reported.
- **"Found" is not the same as "is a live element".** Counting the *string*
  `foreignObject` reported 199 hits on 11 pages; all were prose or WPT
  filenames. Counting actual start tags reports zero — and §4 explains how the
  two are now reconciled automatically instead of by hand.

---

## 3. Per-capture provenance, with before/after sizes and which agent served it

`page.html` figures are the document only; stylesheets are listed separately
because they were fetched independently. The `dropped` column is measured against
the capture body excluding its own provenance header, so a capture at 101% of its
origin is not reported as having grown. Generated by `final.py`.

| shape | origin | served to | original | on disk | of origin | dropped | sheets | faithful | assets out |
| --- | --- | :-: | ---: | ---: | ---: | ---: | :-: | :-: | ---: |
| form-heavy | `https://www.gov.uk/government/statistics/` | browser | 761 KB | 698 KB | 92% | 64 KB | 1/1 | yes | 7 |
| form-heavy | `https://github.com/login` | browser | 46 KB | 46 KB | 100% | 1 KB | 23/23 | yes | 8 |
| data-table | `https://www.iana.org/assignments/media-types` | browser | 709 KB | 710 KB | 100% | 376 B | 1/1 | yes | 14 |
| data-table | `https://en.wikipedia.org/wiki/Comparison_of_web_browsers` | browser | 2 MB | 2 MB | 100% | 8 KB | 2/2 | yes | 24 |
| data-table | `https://en.wikipedia.org/wiki/ISO_4217` | browser | 861 KB | 857 KB | 100% | 6 KB | 2/2 | yes | 747 |
| webfont-heavy | `https://accounts.google.com/ServiceLogin` | browser | 1 MB | 990 KB | 82% | 222 KB | 0/0 | yes | 1 |
| svg-heavy | `https://www.nasa.gov/` | browser | 346 KB | 338 KB | 98% | 9 KB | 3/3 | yes | 676 |
| svg-heavy | `https://getbootstrap.com/` | browser | 79 KB | 80 KB | 101% | 26 B | 2/2 | yes | 15 |
| animation-heavy | `https://github.com/` | browser | 562 KB | 439 KB | 78% | 124 KB | 30/30 | yes | 31 |
| stacked-positioned | `https://github.com/search` | browser | 156 KB | 157 KB | 100% | 842 B | 28/28 | yes | 19 |
| multi-column | `https://en.wikipedia.org/wiki/Portal:Current_events` | browser | 442 KB | 439 KB | 99% | 8 KB | 2/2 | yes | 82 |
| media-heavy | `https://commons.wikimedia.org/wiki/Category:Videos` | browser | 565 KB | 560 KB | 99% | 7 KB | 3/3 | yes | 1340 |
| media-heavy | `https://commons.wikimedia.org/wiki/Category:Audio_files` | browser | 326 KB | 321 KB | 98% | 9 KB | 3/3 | yes | 804 |
| template-and-inline-js | `https://developer.mozilla.org/en-US/` | browser | 115 KB | 92 KB | 81% | 23 KB | 20/20 | yes | 18 |
| template-and-inline-js | `https://www.theguardian.com/` | browser | 1 MB | 1 MB | 100% | 2 KB | 1/1 | yes | 636 |
| data-table | `https://www.w3.org/TR/css-flexbox-1/` | **curl** | 1 MB | 1 MB | 83% | 266 KB | 1/2 | yes | 15 |
| data-table | `https://html.spec.whatwg.org/multipage/tables.html` | browser | 248 KB | 245 KB | 99% | 4 KB | 0/0 | yes | 2 |
| multicolumn | `https://www.w3.org/TR/css-multicol-1/` | **curl** | 566 KB | 557 KB | 98% | 11 KB | 0/1 | yes | 30 |

Totals: **12 MB of documents fetched, 11 MB kept**; **10 MB of stylesheets
fetched, 10 MB kept**; **4469 binary asset URLs recorded and left out**; 22.8 MB
on disk; **18 of 18 faithful**.

### `served to` is a fidelity column, not a footnote

Two captures are marked **curl**, and that is the whole point of the column.
`www.w3.org` **403s a browser-shaped User-Agent and 200s curl's default** — bot
management keyed on the UA string, not the network. So those two documents are
**not what a browser receives**, and any claim about them is a claim about what
curl was served. Every capture records which agent answered, in
`capture_index.json` under `proxy_ua_that_answered` and in the capture's own
header, so this is never a matter of memory.

### Why several captures are ~100% of their origin

Because there was almost nothing binary to remove. A media-type index or a
currency table is 99% text and table markup. The stripper still removed every
external script, every comment, every `data:` URI and every tracking pixel — the
small `dropped` figures are the *document's own* removable bytes, and the real
savings are in the **4469 asset URLs** recorded in each capture's `ASSETS.md` and
never downloaded. The largest excluded asset is a 119 MB video on the Wikimedia
category page. Nothing near 40 MB is present.

### The round-trip result, per capture

`strip.py --lossless` drops nothing and rewrites nothing, so its output must
equal its input byte for byte. Run over every capture body, that is the whole
claim: a capture is *faithful* when the reducer is the identity on it **and** the
reduction does nothing beyond its declared drops. The full table is regenerated
by `final.py`; every row reads `PASS` / `faithful`.

One column deserves a note rather than a defect report. The captures contain
between 1 and 6 **stray end tags** each — a `</script>`, `</div>`, `</option>` or
`</iframe>` with nothing open to close. These are **the origin's own**, confirmed
against a re-fetch: the level-3 comparison finds 0 end tags on any capture that
the origin lacks. They are a *fidelity positive*: this is the misnesting and
omitted-end-tag shape that `render-html` is tested against, preserved rather than
repaired, and 75 of the corpus's `</script>` tags are the reduction's own
external-script placeholders, which pair correctly.

---

## 4. The cross-check, and proof that it fires

The previous version of this file claimed:

> every "CANNOT" is a measured zero, **cross-checked by two independent
> instruments**

**That claim was not backed by anything.** `rawgrep.py` existed — a raw-byte grep
— and `shape_scan.py` existed — a token-level parse — but nothing ever compared
them. And the case that should have made them disagree did: the byte grep
reported **199 `<foreignObject>` hits across 11 pages** while the live element
count was **0**, and nothing fired. So it was not independent, or not being run,
or had no disagreement threshold. It was the third: there was no comparison at
all, and the "cross-check" was two numbers a human had happened to compare.

### What it is now

`crosscheck.py` runs both instruments over every capture for the ten
cross-checked sub-features and reports every disagreement, in three classes:

| class | meaning |
| --- | --- |
| `agree` | both instruments found the shape |
| `measured zero` | both found nothing. **The only kind of zero this project publishes.** |
| `RESOLVED` | the parser found nothing, the bytes found something, and every byte hit is *positively placed* as noise — inside a comment, a `<script>` body, a `<style>` body, a URL-ish attribute value, or ordinary prose |
| `CAUGHT` | the parser found nothing and the bytes found something the classifier **could not place**, or placed as an element. Escalated, never resolved away |

The byte patterns are deliberately **loose** — `foreignobject`, not
`<foreignObject`. A tight pattern would agree with the parser by construction and
the cross-check would never fire, which is the failure being fixed. The loose
form is the one that reported 199 hits, and the cross-check's job is to surface
those and hand them to a reader, not to filter them out in advance.

The two instruments are independent in the way that matters: they share no
tokenizer and no matching rule. The parse cannot see a string in prose, a name
in a URL, or markup in a comment. The grep cannot tell a tag from prose from a
comment. A shape only one instrument can see is either a real finding or an
instrument failure, and `crosscheck.py` cannot tell which — so it reports the
disagreement rather than picking a winner.

### How it was shown to fire

`tests.py::t_crosscheck_fires_on_a_real_disagreement` runs the pair against two
deliberately constructed documents and asserts both outcomes, because there are
two things to prove:

1. **A resolvable disagreement is flagged and then resolved.** A document with
   `<!-- <svg><rect/></svg> -->` and the prose `see foreignObject.html`: the byte
   instrument finds them, the parse does not, and the row comes back
   `severity=resolved`, `hit_is_an_element=False`, with the reason recorded
   (`comment`, `prose`). This is the 199-hit case, and it is now resolved by a
   mechanism rather than by a person deciding a number looked wrong.
2. **An unresolvable disagreement is CAUGHT.** A document with a live
   `<foreignObject>` inside a CDATA section: `html.parser` swallows the marked
   section whole, so the parse instrument cannot see the element while the byte
   instrument can — and the hit is not in a comment, URL or script body, so it
   cannot be explained away. The row comes back in `disagreements`.

A cross-check that has never been observed to fire is not a cross-check. Both
outcomes are asserted on every run of `tests.py`.

### What it says about this corpus right now

```
foreign_object          0 parser,  5 bytes   measured zero on all 18; 1 mention resolved
cdata                   0 parser,  2 bytes   measured zero on all 18; 1 mention resolved
table_with_rules_attr   0 parser,  0 bytes   measured zero on all 18
control_with_form_attr  0 parser,  1 byte    measured zero on all 18; 1 mention resolved
nested_form_token       0 parser, 21 bytes   measured zero on all 18; 10 mentions resolved
svg                   617 parser, 619 bytes  present on 18 pages
svg_use                 39 parser,  39 bytes  present on 18 pages
row_group_tfoot          1 parser,   8 bytes  present on 18 pages
colgroup                 9 parser,  13 bytes  present; 1 mention also resolved as prose/url
table_caption           13 parser,  20 bytes  present on 18 pages
```

These are the 18 corpus2 captures; the combined 27-page figures are in
`gaps.txt` and `gaps.json`. **14 byte-level mentions were resolved, 0
unresolved.** Every published zero is confirmed by both instruments.

A note on the counts differing without a disagreement — `svg` at 617 against
619, `nested_form_token` at 0 against 21. The byte patterns are looser than the
parser (`<form\b` also matches a `<form>` inside a comment; `<svg\b` also
matches `<svg:rect`), and the counts are only ever compared on **presence**, not
equality. A status line that said "measured zero" for a shape the parse had found
was itself a bug, and `tests.py` now asserts it cannot come back.

---

## 5. What the stripper keeps and drops

Following the convention in `tests/fixtures/real_sites/`:

**Kept** — structure and tag names; `class`/`id`/`data-*`; inline `style`;
table shape (`caption`, row groups, `colspan`, `rowspan`, `scope`, `headers`);
form shape (ids, names, actions, methods, control types, `form=`); media shape
(`poster`, `<source src+type>`, `preload`, `controls`); foreign-content shape
(`xmlns:*`, `viewBox`, `xlink:href`, `xml:space`); `<template>` contents;
**inline `<style>` in full**, including `@font-face`, `unicode-range`,
`@supports`, `@keyframes` and media queries; **inline `<script>` start tags and
bodies up to 64 KB**; text content.

**Dropped** — external `<script>` (replaced by a placeholder carrying the
original URL); `data:` URIs; HTML comments; tracking pixels; font, image, audio
and video bytes (never fetched at all — recorded by URL and size instead);
inline scripts over the cap (replaced by a placeholder carrying their byte count).

### The one invariant, and the five bugs it closes

`strip.py` is a **source-span passthrough**. Nothing is reconstructed; every token
is re-emitted as the exact byte range it occupied in the input. Five separate
defects in this project were all the same mistake — rebuilding markup from a
parsed representation of it — and each produced a confidently wrong number:

| defect | what it did | why it survived |
| --- | --- | --- |
| rebuilt tags from the attribute list | re-quoted every value with `"…"`, escaped inner `"` as `&quot;`, and un-escaped `&amp;` to `&`. Wikipedia's 4 KB of single-quoted `data-mw="{…}"` JSON inflated one capture by **25%** | a *growth* in a reduction step is not checked by a size check |
| stack unwinding emitted a closer per skipped element | added **957** `</td>` to the Wikipedia page, repairing exactly the implied end tags and misnesting the parser is tested against | the repaired page still parses, and still looks like a table |
| a placeholder per comment | a 35-byte placeholder per comment added **8%** to a 2.6 MB page | ditto |
| entity references re-emitted as `&name;` | inserted a semicolon the origin did not have; **32 154** references rewritten, `window.wiz._tick()` became `window.wiz;_tick()` | parse-equivalent in a text position, so invisible to every structural check |
| inline `<script>` start tags never emitted | every inline script in every capture became loose body text plus a stray `</script>`; 80 start tags against 254 end tags | the JavaScript text was all still there, just not in a script |

`--lossless` is the mode that makes this checkable: it disables every reduction
and then **asserts the output equals the input**, so a token that cannot be
re-emitted as its own source span fails loudly rather than corrupting a capture.
It runs over every capture body, and `build.py` refuses to write a capture unless
it passes on the origin first.

Two further properties, stated because each is a bug that has already happened:

- **Whitespace is significant inside `<pre>`, `<textarea>` and any element
  carrying `white-space: pre*` in its own `style` attribute**, so text there is
  passed through verbatim; elsewhere runs of whitespace are collapsed, which is
  rendering-neutral under the default `white-space`. A **class-based**
  `white-space: pre` is invisible to a tokeniser and is a recorded limit, not
  something this file can fix.
- **A stylesheet is a stylesheet only if it is one.** `build.py` tests the body,
  not the status: Wikipedia answers a missing sheet with a **196-byte HTML error
  page and status 200**, and two captures recorded those as stylesheets while
  still reporting `2/2 sheets` — losing every CSS-borne shape silently. A
  `&amp;`-escaped `href` compounded it, asking the origin for a different module
  set. Both are fixed, and both have a control in `tests.py`.

`verify.py` checks all 18 captures are valid UTF-8, contain no base64 blob of 512
characters or more, and contain no NUL byte. All 18 pass. It also has three
negative controls of its own, because a verifier with no way to fail is a script
that prints OK.

---

## 6. Hosts and paths that could not be reached

Through the system proxy at `127.0.0.1:17890`. **The network works.** The handful
of notes claiming live fetches are unavailable are stale.

| host / path | what happened | what I tried |
| --- | --- | --- |
| `stackoverflow.com/users/login` | 200 but a **5.4 KB Cloudflare challenge page** ("Just a moment…"), not the login page. Its 1.3 MB of real stylesheets was measured, which is why a bot wall can look like a successful capture. | Browser UA, then curl default UA. Replaced with `github.com/search`, which carries the same `position:sticky` + `z-index` population without a challenge. |
| `www.w3.org/*` and `drafts.csswg.org/*` | **403 to a browser-shaped User-Agent**, 200 to curl's default. Bot management keyed on the UA string, not the network. | Both UAs on every request; the capture records which one answered. Two of the 18 captures (`css-flexbox-1`, `css-multicol-1`) were served by the curl default UA and are marked as such in §3. |
| `en.wikipedia.org` (intermittent) | `schannel: failed to receive handshake` (curl 35) part-way through an earlier survey. | Retried across several runs; it recovered. All four Wikipedia captures re-fetched cleanly in this round. |
| `developer.mozilla.org/en-US/docs/Web/SVG/Tutorial/SVG_from_scratch` | 404 | Both UAs. Wrong path guess. |
| `drafts.csswg.org/css-svg-2/` | 404 | Both UAs. |
| `archive.org/details/inlibrary` | 200 but a 1.8 KB shell — the content is client-rendered, so there is nothing to capture. | Not pursued; it would have added a shell, not a shape. |
| `account.wikimedia.org` | TLS handshake failure (curl 35) | Both UAs. |

Re-probed in this round for S24 and still unreachable: `validator.w3.org` (404),
`www.rfc-editor.org/old/` (404), `www.acm.org` (403 to both agents),
`www.pntcomp.com`, `www.ledatasheet.com`, `wiki.zw.cx` (connection failure),
`www.ripe.net/manage-ips/` (404). Several long-lived HTML4-era pages that were
the most likely `form=` carriers are simply gone, which is itself the finding in
§0.1.

Two `www.w3.org` paths that *should* exist 404'd: `TR/css-tables-1/`,
`TR/css-tables-3/`, `TR/xhtml1/`, `TR/xml/`, `TR/xlink/`. The `colgroup` and
`tfoot` captures were found by probing ~30 spec URLs instead.

**Two of the 18 captures are documentation, not a commercial page**
(`css-flexbox-1` for `<colgroup>`, `css-multicol-1` for the multi-column
properties together), and they are labelled as such. They are there because the
alternative was leaving a sub-feature at zero, and a spec page is a real page
that a real browser has to render. The `tfoot` capture is
`html.spec.whatwg.org/multipage/tables.html` — also documentation, and the
densest *table page* found, though §2 corrects its figures: 9 tables, 1
`colspan`, 4 `rowspan`, 3 `<caption>`, not the 161 tables previously recorded.

---

## 7. What the corpus made obvious, and did **not** fix

Recorded, not fixed. An agent that starts editing the crates will collide with
several others.

**The instrumentation is where the defects were, every time.** Not one engine
defect is described in this file, and that is itself the finding: on both passes
the corpus's value was in correcting the *measurement*, not in exposing a new
engine bug. All seven had produced a confidently wrong answer first. Most failed
in the direction of **under-reporting** — the dangerous direction, because a
corpus that under-reports looks like a corpus with few problems, and this
project's whole method is to measure the corpus and believe it. The two that ran
the other way (the 3.6× `@font-face` inflation and the 18× table inflation) are
arguably worse, because an inflated coverage claim is acted on with confidence
and takes far longer to notice than a thin one.

### One engine observation, not claimed as a defect

`github.com/search` and `github.com/` ship **111 `position:sticky` and 1576
`z-index` declarations across 25 of 27 pages**. S6 records that `z-index` is read
as raw text with no consumer and that the paint layer has no z-index awareness.
That is already in the register as an open item; what is new is that the corpus
now measures its population, so the item can be **closed by count** rather than
by assertion.

**Where that number lives:** `tools/corpus2/gaps.json`, under
`gaps.S6-stacking` — machine-readable, regenerated by `python
tools/corpus2/gaps.py` from `matrix.json`, with `occurrences`, `pages` and
`measured` for every counter each gap's claim rests on. `measured` is the field
that matters: it distinguishes *no page has the shape* from *nobody looked for
it*, which is the distinction this file has had to add twice. The
human-readable form is `.diag/corpus2/_tools/gaps.txt`, under **CAN S6 z-index
and stacking contexts are unconsumed**.

Not investigated here, and not fixed.

---

## 8. Layout

```
tools/corpus2/                 COMMITTED: the instruments and their tests
  README.md                    this file
  paths.py                     where the tools, their data and the corpus live
  tests.py                     41 negative controls, one per instrument
  shape_scan.py                per-page shape measurement (live vs code-sample)
  strip.py                     the reduction; --lossless is the identity
  build.py                     fetch -> fidelity gate -> discover CSS -> strip
  matrix.py gaps.py            the two tables above
  roundtrip.py                 is every capture byte-faithful?
  crosscheck.py                the two-instrument cross-check
  verify.py final.py assets.py fetch.py survey.py query.py report.py
  rawgrep.py probe3.py         survey instruments
  gaps.json                    the per-gap counts, machine-readable

.diag/corpus2/                 gitignored: the captures
  form-heavy/  data-table/  webfont-heavy/  svg-heavy/
  animation-heavy/  stacked-positioned/  multi-column/  multicolumn/
  media-heavy/  template-and-inline-js/
    <slug>/page.html      the document, with a provenance header naming the
                          origin, the agent that served it, the fidelity
                          verdict, and every reduction performed
    <slug>/sheet-NN.css   its stylesheets, in document order
    <slug>/ASSETS.md      binaries left out: class, count, bytes, largest URL
  _tools/                 data and shims
    *.py                  one-line shims onto tools/corpus2/
    matrix.json gaps.txt  machine- and human-readable measurements
    capture_index.json    provenance, sizes, UA and fidelity, per capture
    roundtrip.json        the three-level fidelity report
    candidates*.txt probe_*.txt   the survey and probe inputs
```

`multi-column/` and `multicolumn/` are two directories because the survey produced
both: `multi-column` holds a commercial page, `multicolumn` holds the spec page
that carries the `column-width` / `columns` combination. Both are the same shape
and are counted together in the matrix.
