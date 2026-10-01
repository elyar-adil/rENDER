#!/usr/bin/env python3
"""Shape coverage matrix over the combined corpus.

Measures every HTML capture in `.diag/` (read-only) and every capture in
`.diag/corpus2/`, and reports, per shape: which captures carry it, how
deeply, and the specific sub-features that are absent everywhere.

Sub-feature depth is the point. A shape scored "represented" by one
incidental hit is a shape the corpus cannot actually exercise, so the matrix
lists the discriminating sub-features separately and marks the ones with zero
pages, rather than collapsing everything into one number.
"""

from __future__ import annotations

import json
import os
import sys
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402

CORPUS = paths.CORPUS
DIAG = paths.DIAG

sys.path.insert(0, HERE)
import shape_scan  # noqa: E402

SHAPES = [
    "form_heavy", "data_table", "webfont_heavy", "svg_heavy",
    "animation_heavy", "stacked_positioned", "multicolumn",
    "media_heavy", "template_and_inline_js",
]

# The sub-features that make each shape worth having. A shape is only
# "represented" if at least one of these is present on a real page.
SUBFEATURES = {
    "form_heavy": [
        ("form element", "form"),
        ("input type=password (a login)", "input_type_password"),
        ("input type=search", "input_type_search"),
        ("input type=submit", "input_type_submit"),
        ("input type=checkbox", "input_type_checkbox"),
        ("input type=radio", "input_type_radio"),
        ("control outside every form", "listed_control_outside_form"),
        ("control with form= attribute", "control_with_form_attr"),
        ("nested <form> start tag (parser form pointer)", "nested_form_token"),
        ("<select>", "select"),
        ("<textarea>", "textarea"),
        ("<fieldset>", "fieldset"),
        ("<label>", "label"),
        ("<button>", "button"),
    ],
    "data_table": [
        ("<caption>", "table_caption"),
        ("<thead>", "row_group_thead"),
        ("<tbody>", "row_group_tbody"),
        ("<tfoot>", "row_group_tfoot"),
        ("colspan", "cell_colspan"),
        ("rowspan", "cell_rowspan"),
        ("th[scope]", "th_with_scope"),
        ("<colgroup>", "colgroup"),
        ("table[rules]", "table_with_rules_attr"),
    ],
    "webfont_heavy": [
        ("@font-face", "font_face"),
        ("unicode-range", "unicode_range"),
        ("font-display", "font_display"),
        ("font-variation-settings", "font_variation_settings"),
        ("font-feature-settings", "font_feature_settings"),
    ],
    "svg_heavy": [
        ("inline <svg>", "svg"),
        ("viewBox", "svg_viewbox"),
        ("xlink:href", "xlink_href"),
        ("xml:space", "xml_space"),
        ("xmlns:* declaration", "xmlns_declared"),
        ("<svg:use>", "svg_use"),
        ("<foreignObject> element", "foreign_object"),
        ("CDATA section", "cdata"),
    ],
    "animation_heavy": [
        ("@keyframes", "keyframes"),
        ("transition", "transition_prop"),
        ("transition-delay", "transition_delay"),
        ("animation", "animation_prop"),
        ("animation-delay", "animation_delay"),
        ("animation-fill-mode", "animation_fill_mode"),
    ],
    "stacked_positioned": [
        ("position: sticky", "position_sticky"),
        ("position: fixed", "position_fixed"),
        ("position: absolute", "position_absolute"),
        ("z-index", "z_index"),
        ("transform", "transform"),
        ("isolation", "isolation"),
        ("mix-blend-mode", "mix_blend_mode"),
        ("will-change", "will_change"),
    ],
    "multicolumn": [
        ("float", "float"),
        ("clear", "clear"),
        ("column-count", "column_count"),
        ("column-width", "column_width"),
        ("column-gap", "column_gap"),
        ("columns shorthand", "columns"),
    ],
    "media_heavy": [
        ("<video>", "video"),
        ("<audio>", "audio"),
        ("poster", "video_poster"),
        ("<source type=...>", "source_with_type"),
        ("controls", "video_controls"),
        ("preload", "video_preload"),
    ],
    "template_and_inline_js": [
        ("<template>", "template"),
        ("template containing a row tag", "template_containing_row"),
        ("inline script binding a load handler", "inline_script_binds_load"),
    ],
}


def measure(path, css_dir=None):
    doc, nb, live = shape_scan.scan_html(path)
    c = Counter(doc.c)
    c.update(live)
    css_bytes = 0
    # The page's own inline <style> blocks and style="" attributes. Omitting
    # these reported webfont_heavy for one capture as 21 when it carries 273
    # @font-face blocks in an inline sheet - the measurement being wrong, not
    # the page.
    with open(path, "rb") as fh:
        page_text = fh.read().decode("utf-8", "replace")
    # scan_page_css, not scan_css: neither a code sample nor a <script> body is
    # a page using that CSS. The difference is not academic - one capture
    # reported 388 `transition` declarations, most of them inside inline
    # JavaScript, and the inline-JS shape is one of the nine targets.
    inline = Counter(shape_scan.scan_page_css(page_text))
    c.update(inline)
    css_bytes += len(page_text)
    if css_dir and os.path.isdir(css_dir):
        for f in sorted(os.listdir(css_dir)):
            if not f.endswith(".css"):
                continue
            with open(os.path.join(css_dir, f), "rb") as fh:
                t = fh.read().decode("utf-8", "replace")
            c.update(shape_scan.scan_css(t))
            css_bytes += len(t)
    scores = shape_scan.shape_score(c)
    return c, scores, nb, css_bytes


def main():
    pages = []
    # existing .diag corpus, read-only
    for root, dirs, files in os.walk(DIAG):
        dirs[:] = [d for d in dirs if d != "corpus2"]
        for f in files:
            if f.endswith(".html"):
                p = os.path.join(root, f)
                rel = os.path.relpath(p, DIAG).replace("\\", "/")
                try:
                    c, sc, nb, cb = measure(p, os.path.dirname(p))
                except Exception as exc:
                    print("skip %s: %s" % (rel, exc), file=sys.stderr)
                    continue
                pages.append({"label": "existing:" + rel, "origin": "existing",
                              "counts": c, "scores": sc, "html_bytes": nb,
                              "css_bytes": cb})
    # corpus2
    for shape in sorted(os.listdir(CORPUS)):
        sd = os.path.join(CORPUS, shape)
        if not os.path.isdir(sd) or shape.startswith("_"):
            continue
        for cap in sorted(os.listdir(sd)):
            cd = os.path.join(sd, cap)
            hp = os.path.join(cd, "page.html")
            if not os.path.exists(hp):
                continue
            c, sc, nb, cb = measure(hp, cd)
            pages.append({"label": cap, "origin": "corpus2", "shape": shape,
                          "path": os.path.relpath(hp, DIAG).replace("\\", "/"),
                          "counts": c, "scores": sc, "html_bytes": nb,
                          "css_bytes": cb})

    out = {"pages": [{"label": p["label"], "origin": p["origin"],
                      "shape": p.get("shape"), "path": p.get("path"),
                      "html_bytes": p["html_bytes"], "css_bytes": p["css_bytes"],
                      "scores": p["scores"],
                      "counts": {k: v for k, v in sorted(p["counts"].items()) if v},
                      # Counters this instrument looked for and found nothing,
                      # listed so a published zero can be told apart from a key
                      # nobody checked - the difference between "no page has the
                      # shape" and "nobody looked for it".
                      "measured_zero": sorted(k for k, v in p["counts"].items()
                                              if not v)}
                     for p in pages]}

    print("=" * 96)
    print("pages measured: %d (%d existing .diag, %d corpus2)"
          % (len(pages), sum(1 for p in pages if p["origin"] == "existing"),
             sum(1 for p in pages if p["origin"] == "corpus2")))
    for shape in SHAPES:
        print("=" * 96)
        ranked = sorted(pages, key=lambda p: -p["scores"].get(shape, 0))
        top = ranked[0]["scores"].get(shape, 0) if ranked else 0
        carried = [p for p in pages if p["scores"].get(shape, 0) > 0]
        print("SHAPE %-22s score=%-6d pages carrying it: %d/%d"
              % (shape, top, len(carried), len(pages)))
        print("  sub-feature presence across the WHOLE combined corpus:")
        for name, key in SUBFEATURES[shape]:
            tot = sum(p["counts"].get(key, 0) for p in pages)
            npages = sum(1 for p in pages if p["counts"].get(key, 0) > 0)
            if tot == 0:
                print("    %-42s ABSENT  (0 occurrences, 0 pages)" % name)
            else:
                print("    %-42s %-7d on %d pages" % (name, tot, npages))
        best = [p for p in carried[:5]]
        if best:
            print("  deepest carriers:")
            for p in best:
                print("    %-7s %-50s score=%d" % (
                    p["origin"], p["label"][:50], p["scores"].get(shape, 0)))

    with open(paths.data("matrix.json"), "w", encoding="utf-8") as fh:
        json.dump(out, fh, indent=1, sort_keys=True)
    print("\nwrote matrix.json")

    # Keep capture_index.json's per-capture scores in step with the matrix.
    # Two files holding the same number is one number too many: the index is
    # written by build.py, the matrix by here, and they drift the moment an
    # instrument is fixed. The matrix is authoritative, so it writes back.
    idx_path = paths.data("capture_index.json")
    if os.path.exists(idx_path):
        with open(idx_path, encoding="utf-8") as fh:
            idx = json.load(fh)
        by_path = {p["path"]: p for p in out["pages"] if p.get("path")}
        n = 0
        for rec in idx:
            p = by_path.get("%s/%s/page.html" % (rec.get("shape"),
                                                 rec.get("slug")))
            if not p:
                continue
            if rec.get("stripped_scores") != p["scores"]:
                rec["stripped_scores"] = p["scores"]
                n += 1
            rec["stripped_counts"] = p["counts"]
            rec["measured_by"] = "matrix.py (authoritative); build.py's own " \
                                 "scan is superseded by this"
        with open(idx_path, "w", encoding="utf-8") as fh:
            json.dump(idx, fh, indent=1, sort_keys=True)
        print("resynced %d capture_index score(s) from the matrix" % n)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
