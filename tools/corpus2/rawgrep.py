#!/usr/bin/env python3
"""Raw-byte grep across every captured body.

A negative claim ("no page in the corpus contains <foreignObject>") must not
rest on a parser that might simply have mis-tokenised it. This greps the raw
bytes, case-insensitively, for the shapes that came back zero.
"""
from __future__ import annotations
import json, os, re, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402

SCRATCH = paths.SCRATCH
PROV = paths.data("provenance_survey.json")

PATTERNS = {
    "form_attr": re.compile(rb"<[a-zA-Z][^>]{0,400}?\sform\s*=", re.I),
    "nested_form": re.compile(rb"<form\b", re.I),
    "form_start_tag": re.compile(rb"<form\b[^>]*>", re.I),
    "foreignObject": re.compile(rb"foreignobject", re.I),
    "cdata": re.compile(rb"<!\[CDATA\[", re.I),
    "template": re.compile(rb"<template\b", re.I),
    "xlink_href": re.compile(rb"xlink:href", re.I),
    "xml_space": re.compile(rb"xml:space", re.I),
    "svg_tag": re.compile(rb"<svg\b", re.I),
    "poster": re.compile(rb"\bposter\s*=", re.I),
    "source_type": re.compile(rb"<source\b[^>]*\btype\s*=", re.I),
    "unicode_range": re.compile(rb"unicode-range", re.I),
    "font_face": re.compile(rb"@font-face", re.I),
    "column_count": re.compile(rb"column-count", re.I),
    "position_sticky": re.compile(rb"position\s*:\s*sticky", re.I),
    "keyframe_block": re.compile(rb"@(?:-webkit-|-moz-)?keyframes", re.I),
    "animation_delay": re.compile(rb"animation-delay", re.I),
    "col_span": re.compile(rb"\bcolspan\s*=", re.I),
    "row_span": re.compile(rb"\browspan\s*=", re.I),
    "caption": re.compile(rb"<caption\b", re.I),
    "nested_form_tokens": re.compile(rb"", re.I),
}


def main():
    want = sys.argv[1:] or list(PATTERNS)
    prov = {}
    if os.path.exists(PROV):
        with open(PROV, encoding="utf-8") as fh:
            for r in json.load(fh):
                prov[r["name"]] = r
    bodies = sorted(f for f in os.listdir(SCRATCH) if f.endswith(".body"))
    totals = {w: 0 for w in want}
    pages = {w: [] for w in want}
    for b in bodies:
        path = os.path.join(SCRATCH, b)
        with open(path, "rb") as fh:
            data = fh.read()
        name = b[: -len(".body")]
        url = prov.get(name, {}).get("url", name)
        for w in want:
            n = len(PATTERNS[w].findall(data))
            if n:
                totals[w] += n
                pages[w].append((n, url))
    for w in want:
        print("%-20s total=%-7d pages=%d" % (w, totals[w], len(pages[w])))
        for n, url in sorted(pages[w], reverse=True)[:5]:
            print("      %4d  %s" % (n, url))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
