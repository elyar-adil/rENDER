#!/usr/bin/env python3
"""Print the measured shape profile of every scanned body, in full."""
from __future__ import annotations
import json, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402

SCRATCH = paths.SCRATCH
PROV = paths.data("provenance_survey.json")
SCAN = paths.data("scan_survey.json")

KEYS = {
    "form_heavy": ["form", "nested_form_token", "listed_control",
                   "listed_control_inside_form", "listed_control_outside_form",
                   "control_with_form_attr", "form_attr_reaches_outside_form",
                   "form_attr_names_non_ancestor_form", "input_type_password",
                   "input_type_search", "input_type_submit", "select", "textarea"],
    "data_table": ["table", "table_caption", "row_group_thead", "row_group_tbody",
                   "row_group_tfoot", "table_row", "table_cell", "cell_colspan",
                   "cell_rowspan", "th_with_scope", "colgroup", "col",
                   "table_with_rules_attr"],
    "webfont_heavy": ["font_face", "unicode_range", "font_display",
                      "font_variation_settings", "font_feature_settings",
                      "supports", "media_rule"],
    "svg_heavy": ["svg", "xlink_href", "xml_space", "xml_other", "xmlns_declared",
                  "foreign_object", "cdata", "svg_viewbox"],
    "animation_heavy": ["keyframes", "transition_prop", "transition_delay",
                        "animation_prop", "animation_delay", "animation_fill_mode"],
    "stacked_positioned": ["position_sticky", "position_fixed", "position_absolute",
                           "z_index", "transform", "will_change", "isolation",
                           "mix_blend_mode"],
    "multicolumn": ["float", "clear", "column_count", "columns", "column_width",
                 "column_gap"],
    "media_heavy": ["video", "audio", "video_poster", "audio_poster", "source",
                    "source_with_type", "video_controls", "audio_controls",
                    "video_preload", "video_autoplay"],
    "template_and_inline_js": ["template", "inline_script_binds_load"],
}


def main():
    with open(SCAN, encoding="utf-8") as fh:
        rows = json.load(fh)
    prov = {}
    if os.path.exists(PROV):
        with open(PROV, encoding="utf-8") as fh:
            for r in json.load(fh):
                prov[r["name"]] = r
    want = sys.argv[1:] or list(KEYS)
    for shape in want:
        print("=" * 100)
        print("SHAPE", shape)
        ranked = sorted(rows, key=lambda r: -r["scores"].get(shape, 0))
        for r in ranked[:8]:
            if r["scores"].get(shape, 0) <= 0:
                break
            name = r["file"].replace(".html", "")
            url = prov.get(name, {}).get("url", "?")
            print("\n  score=%-7d %s" % (r["scores"][shape], url))
            print("    html=%d B  css=%d B/%d sheets  inline-style=%d B  "
                  "scripts: %d inline (%d B) + %d external"
                  % (r["html_bytes"], r["css_bytes"], r["css_files"],
                     r["inline_style_bytes"], r["inline_scripts"],
                     r["inline_script_bytes"], r["external_scripts"]))
            bits = []
            for k in KEYS[shape]:
                v = r["counts"].get(k, 0)
                if v:
                    bits.append("%s=%d" % (k, v))
            print("    " + "  ".join(bits))


if __name__ == "__main__":
    raise SystemExit(main())
