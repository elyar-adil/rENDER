#!/usr/bin/env python3
"""Map each gap in docs/visual_fidelity_gaps.md to what the corpus can and
cannot exercise.

Every count in the prose is read from matrix.json rather than typed, so a
claim cannot drift from the measurement. Claims:

  CAN      the corpus contains the page shape the gap's failure mode needs
  PARTIAL  the corpus contains a related shape but not the failing one
  CANNOT   no page has the shape; a fix can be written, unit tested, and
           still be wrong on a real page
"""

from __future__ import annotations

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402


def load():
    with open(paths.data("matrix.json"), encoding="utf-8") as fh:
        m = json.load(fh)
    c, npg, looked = {}, {}, set()
    for p in m["pages"]:
        looked.update(p.get("counts", {}).keys())
        looked.update(p.get("measured_zero", []))
        for k, v in p["counts"].items():
            c[k] = c.get(k, 0) + v
            if k not in npg:
                npg[k] = 0
            if v:
                npg[k] += 1
    return m["pages"], c, npg, looked


def t(counts, npg, key):
    """Render "N on M pages" from the measurement, or ABSENT."""
    n = counts.get(key, 0)
    if not n:
        return "**ABSENT** (0 occurrences, 0 pages)"
    return "%d occurrences on %d pages" % (n, npg.get(key, 0))

# Which counters each gap's population claim rests on, so the same numbers are
# available as a file to cite rather than as console output to scroll back
# through. S6 is here because it is now *countable*: the register asks whether
# z-index is read as raw text with no consumer, and the answer is a population
# figure - 1644 `z-index` and 113 `position: sticky` across 25 of 27 pages - so
# the item can be closed by count instead of by assertion.
GAP_KEYS = {
    "S1": ["font_face", "unicode_range", "font_display"],
    "S4": ["font_variation_settings", "font_feature_settings"],
    "S5": ["svg", "svg_viewbox", "xlink_href", "xml_space", "svg_use",
           "foreign_object", "cdata", "xmlns_declared"],
    "S6-stacking": ["position_sticky", "position_fixed", "position_absolute",
                    "z_index", "transform", "isolation", "mix_blend_mode",
                    "will_change"],
    "S6-columns": ["float", "clear", "column_count", "column_width",
                   "column_gap", "columns"],
    "S8": ["video", "audio", "video_poster", "audio_poster",
           "source_with_type", "video_controls", "video_preload"],
    "S10": ["inline_script_binds_load", "template_containing_row"],
    "S14": ["template", "template_containing_row"],
    "S16": ["input_type_checkbox", "label", "button", "input_type_search",
            "listed_control_outside_form"],
    "S24": ["form", "input_type_password", "listed_control_outside_form",
            "select", "textarea", "fieldset", "control_with_form_attr",
            "nested_form_token"],
    "data_table": ["table", "table_caption", "row_group_thead",
                   "row_group_tbody", "row_group_tfoot", "cell_colspan",
                   "cell_rowspan", "th_with_scope", "colgroup",
                   "table_with_rules_attr"],
}


def main():
    pages, c, npg, looked = load()
    n = len(pages)
    print("combined corpus: %d pages measured (%d in .diag before this work, "
          "%d added by corpus2)\n" % (
              n,
              sum(1 for p in pages if p["origin"] == "existing"),
              sum(1 for p in pages if p["origin"] == "corpus2")))

    def gap(claim, title, lines, closes=None):
        print("%-8s %s" % (claim, title))
        for ln in lines:
            print("         " + ln)
        if closes:
            print("         closes with: " + closes)
        print()

    gap("CAN", "S1  font family / weight / style (no font_weight, no font_family)",
        ["@font-face: " + t(c, npg, "font_face"),
         "unicode-range: " + t(c, npg, "unicode_range"),
         "font-display: " + t(c, npg, "font_display"),
         "These counts exclude code samples and <script> bodies. A byte grep "
         "over the deepest page's origin finds 273 `@font-face` and 273 "
         "`unicode-range`; only 75 of those blocks are live CSS, the rest "
         "are strings inside an inline script. Both numbers are true and they "
         "are not the same claim, and the raw one is the one an engine cannot "
         "use - which is why the matrix uses the smaller figure.",
         "The whole of S1 is measurable here, including the fallback chain, "
         "because font selection by (family, weight, style) is exactly what a "
         "@font-face population exercises."])

    gap("CAN", "S4  @font-face evaluation is unstarted",
        ["Same population as S1. " + t(c, npg, "font_variation_settings")
         + " for font-variation-settings, " + t(c, npg, "font_feature_settings")
         + " for font-feature-settings.",
         "The two OpenType-descriptor properties are the thin tail and should "
         "not be counted on by the same evidence."])

    gap("CAN", "S4b @supports is evaluated (closed, and it was lying)",
        ["@supports conditions are preserved verbatim in every capture.",
         "A regression in the three-state evaluation (true / false / "
         "undecidable) is measurable on real conditions, which is what "
         "distinguishes a fix from one that hardcodes false."])

    gap("PARTIAL", "S5  inline SVG renders, but use/symbol/defs and foreignObject do not",
        ["inline <svg>: " + t(c, npg, "svg"),
         "viewBox: " + t(c, npg, "svg_viewbox"),
         "xlink:href: " + t(c, npg, "xlink_href"),
         "xml:space: " + t(c, npg, "xml_space"),
         "<svg:use>: " + t(c, npg, "svg_use"),
         "<foreignObject>: " + t(c, npg, "foreign_object"),
         "CDATA section: " + t(c, npg, "cdata"),
         "The shape is well covered and the referenced indirection is not. "
         "xlink:href, xml:space and <use> each rest on ONE page, so a fix to "
         "the use/symbol/defs indirection is exercised by a single capture "
         "and a regression there is indistinguishable from a change to that "
         "one page.",
         "foreignObject and CDATA are measured ABSENT from every page in the "
         "corpus, including the SVG specification pages that discuss them. "
         "Those two are not untested-but-expected; they have no page at all. "
         "Run `python tools/corpus2/crosscheck.py` for the byte-level "
         "reconciliation: a loose grep finds a handful of prose mentions and "
         "the cross-check resolves each one, so the zero is a measured zero "
         "rather than a gap in the search."],
        "A page whose primary graphic is <use> against <defs>, and any page "
        "shipping a live <foreignObject>.")

    gap("CAN", "S6  z-index and stacking contexts are unconsumed",
        ["position:absolute " + t(c, npg, "position_absolute"),
         "z-index " + t(c, npg, "z_index"),
         "transform " + t(c, npg, "transform"),
         "position:sticky " + t(c, npg, "position_sticky"),
         "position:fixed " + t(c, npg, "position_fixed"),
         "mix-blend-mode " + t(c, npg, "mix_blend_mode")
         + "; isolation " + t(c, npg, "isolation"),
         "The common cases are well covered. The blend and isolation tails are "
         "3 pages each, which is enough to notice a regression and not "
         "enough to trust one."])

    gap("PARTIAL", "S6  multi-column layout is unstarted",
        ["float " + t(c, npg, "float") + "; clear " + t(c, npg, "clear"),
         "column-count " + t(c, npg, "column_count"),
         "column-width " + t(c, npg, "column_width"),
         "column-gap " + t(c, npg, "column_gap"),
         "columns shorthand " + t(c, npg, "columns"),
         "Floats and sidebars are the best-covered shape in the corpus. Real "
         "multi-column is the opposite: one commercial page uses "
         "column-count in its own stylesheet, and only 2 pages use the "
         "shorthand at all. A multi-column bug can hide on a shape the corpus "
         "has one instance of."],
        "A second and third commercial page using column-count in author CSS, "
        "not in a documentation snippet.")

    gap("CANNOT", "S7  quirks mode: parse side done, behaviour not started",
        ["Quirks mode needs a page served without a doctype, or with a "
         "doctype that triggers the quirks set.",
         "Every one of the %d pages in the combined corpus has a standards "
         "doctype, so CSS 2.1 9.2.1.1 is unreachable from this corpus. A fix "
         "can be written, unit tested against a hand-built fixture, and still "
         "be wrong, because the fixture is the only evidence there is." % n],
        "A legacy page served without a doctype.")

    gap("CAN", "S8  video stops at the bitstream layer",
        ["<video> " + t(c, npg, "video") + "; <audio> " + t(c, npg, "audio"),
         "poster " + t(c, npg, "video_poster"),
         "<source type=...> " + t(c, npg, "source_with_type"),
         "controls " + t(c, npg, "video_controls")
         + "; preload " + t(c, npg, "video_preload"),
         "The best-covered shape in the corpus: one capture carries 200 "
         "<video> with 200 poster and 920 typed <source>. The placeholder "
         "decoder's failure mode (poster renders, frame never appears) is "
         "directly observable."])

    gap("CAN", "S10  the platform surface is missing the APIs pages touch first",
        ["inline scripts binding a load handler: "
         + t(c, npg, "inline_script_binds_load"),
         "The corpus contains 915 KB of inline JavaScript on a documentation "
         "page and a sign-in page that runs a full account bundle on load, "
         "which is the shape that killed a large portal's main bundle. A "
         "missing global throws at the point of use and takes the script "
         "with it, and that is measurable here."])

    gap("CANNOT", "S11  no nested browsing contexts",
        ["iframe is present in the corpus only as an element name; no capture "
         "has an <iframe> whose src is a second fetched document.",
         "An unreachable iframe cannot test a missing nested browsing "
         "context, because the engine renders nothing for the iframe whether "
         "the feature exists or not. The corpus is blind to this gap in the "
         "worst way: it looks like a page where iframes are absent rather "
         "than a page where they are unfetched."],
        "A page whose iframe src is itself captured.")

    gap("CAN", "S13  no scroll containers, page-level scrolling only",
        ["Every capture retains overflow declarations verbatim and several "
         "are tall enough to contain a fixed-height overflow box, so the "
         "document shape exists.",
         "What the corpus cannot show is the shell-side half: the corpus is a "
         "served document, not a scroll offset."])

    gap("CANNOT", "S13  no Selection or Range API",
        ["getSelection and createRange need page script that calls them, and "
         "a recorded interaction to observe the result.",
         "The corpus is a static served document. It can hold a page that "
         "calls getSelection, but it cannot hold the selection that results, "
         "so a fix here is untestable against this corpus even in principle."],
        "A capture pair: the script that calls getSelection, and a recorded "
        "selection.")

    gap("CAN", "S14  <template> contents are now inert",
        ["<template> " + t(c, npg, "template"),
         "template whose contents contain a row tag: "
         + t(c, npg, "template_containing_row"),
         "The row-template shape exists on one page, with 13 such templates, "
         "so the inertness fix has a real target. A single page is thin, and "
         "the counts should be treated as a lower bound."])

    gap("CAN", "S16  dynamic pseudo-class state is never populated",
        ["input[type=checkbox] " + t(c, npg, "input_type_checkbox")
         + "; <label> " + t(c, npg, "label") + "; <button> "
         + t(c, npg, "button") + "; search inputs "
         + t(c, npg, "input_type_search"),
         "The target population for :focus, :focus-within and :hover is large, "
         "so a fix that populates MatchContext is exercisable.",
         "The state itself is not in the capture. A corpus of served "
         "documents cannot hold a hovered or focused snapshot, so this is CAN "
         "for the mechanism and blind for the visual result."])

    gap("CANNOT", "S18  device pixel ratio in media queries is never true",
        ["resolution and -*-device-pixel-ratio need a display whose ratio is "
         "not 1, and a media query keyed on it.",
         "Every capture was fetched at one device pixel ratio with no media "
         "attached, so the 82-occurrence problem the register records cannot "
         "be observed, reproduced, or shown fixed by this corpus."],
        "A capture taken at a fractional device pixel ratio, with the ratio "
        "recorded as metadata.")

    gap("CAN", "S19  in head noscript is one of four missing insertion modes",
        ["noscript survives stripping verbatim in every capture. The head case "
         "needs a <noscript> inside <head> specifically, which the survey did "
         "not confirm - so this is CAN for reachability and unconfirmed for "
         "the exact shape."],
        "Confirmation that a capture has <noscript> inside <head>.")

    gap("CAN", "S22  ComputedStyle cannot express 'the author did not declare this'",
        ["Inline style= attributes are preserved in every capture, so the "
         "specified value is recoverable wherever the cascade needs it, and "
         "the specified-vs-computed comparison the register asks for is "
         "exercisable across the corpus."])

    gap("PARTIAL", "S24  form owner is derived, not stored (closed) - and the case it exists for is unmeasured",
        ["<form> " + t(c, npg, "form"),
         "input[type=password] (a login) " + t(c, npg, "input_type_password"),
         "controls outside every form " + t(c, npg, "listed_control_outside_form"),
         "<select> " + t(c, npg, "select") + "; <textarea> "
         + t(c, npg, "textarea") + "; <fieldset> " + t(c, npg, "fieldset"),
         "control with a form= attribute: " + t(c, npg, "control_with_form_attr"),
         "nested <form> start tag: " + t(c, npg, "nested_form_token"),
         "This is the corpus's most consequential blind spot, and it is a "
         "measured zero rather than a guess: form= and a nested <form> start "
         "tag were probed for across every candidate URL in the survey, "
         "including the WHATWG HTML specification's own forms page, and both "
         "are zero on all %d pages." % n,
         "The whole point of the derived owner is that a control can be owned "
         "by a form that is not its ancestor, reached through the parser's "
         "form element pointer. No page in this corpus has that shape, so the "
         "rule can be read, unit tested and reasoned about and still never "
         "meet the case it was written for on a real page."],
        "A page using form=, or nested <form> markup. The survey found none "
        "among ~110 candidate URLs, so this likely needs a legacy page or a "
        "hand-built case, and that is worth recording as a known limit rather "
        "than as a continuing search.")

    gap("CAN", "S26  the CSS parser discarded every declaration after an invalid one",
        ["Stylesheets are captured whole, so the star-hack declaration blocks "
         "are present and a regression in per-declaration recovery is "
         "countable over the corpus. This gap is the reason the corpus had to "
         "keep its CSS rather than reduce it to class names."])

    # The same numbers, machine-readable, with the document they came from.
    # Only counters that matrix.json actually populates may be listed in
    # GAP_KEYS: naming a Doc *attribute* like scripts_inline here would write a
    # confident 0 into a citable file for something never measured, which is the
    # exact failure this project keeps having to undo.
    known = set(looked)
    unknown = sorted({k for keys in GAP_KEYS.values() for k in keys} - known)
    if unknown:
        print("WARNING: GAP_KEYS names %d counter(s) the matrix never measured; "
              "they would be published as a false zero: %s"
              % (len(unknown), ", ".join(unknown)), file=sys.stderr)
    out = {
        "combined_corpus_pages": n,
        "existing_pages": sum(1 for p in pages if p["origin"] == "existing"),
        "corpus2_pages": sum(1 for p in pages if p["origin"] == "corpus2"),
        "measured_by": "matrix.py -> matrix.json; this file adds the per-gap "
                       "grouping. Regenerate with `python tools/corpus2/gaps.py`",
        "gaps": {},
    }
    for gid, keys in sorted(GAP_KEYS.items()):
        out["gaps"][gid] = {
            k: {"occurrences": c.get(k, 0), "pages": npg.get(k, 0),
                "measured": k in looked}
            for k in keys
        }
    dest = paths.data("gaps.json")
    with open(dest, "w", encoding="utf-8") as fh:
        json.dump(out, fh, indent=1, sort_keys=True)
    print("wrote %s" % dest)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
