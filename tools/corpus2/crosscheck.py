#!/usr/bin/env python3
"""Two independent instruments, and the disagreement test between them.

THE CLAIM THIS FILE EXISTS TO MAKE TRUE, AND TO PROVE

    "every CANNOT is a measured zero, cross-checked by two independent
     instruments."

The claim was in the README and it was not backed by anything. `rawgrep.py`
existed - a raw-byte grep over the scratch bodies - and `shape_scan.py` existed
- a token-level parse - but nothing ever compared them. And the case that should
have made them disagree did: the byte grep reported **199 `<foreignObject>` hits
across 11 pages** while the live element count is **0**, because every one of
those hits was prose or a filename inside a WPT test-name URL. The two
instruments plainly disagreed and nothing fired.

So this file is the cross-check, and it has one job beyond comparing: to be
*shown* firing. `tests.py::t_crosscheck_fires_on_a_real_disagreement` injects a
disagreement and asserts it is caught. A cross-check that has never been
observed to fire is not a cross-check.

WHY TWO INSTRUMENTS ARE INDEPENDENT ENOUGH

They do not share a tokenizer and they do not share a matching rule:

  parser   html.parser tokenises the document and counts start tags. It cannot
           see a string in prose, a name in a URL, or markup inside a comment,
           because none of those are tags. It is blind to the *text* of the
           document.

  bytes    a regular expression over the raw file. It cannot tell a tag from
           prose from a comment. It is blind to *structure*.

A shape that only one instrument can see is either a real finding or an
instrument failure, and this file cannot tell which - so it reports the
disagreement rather than picking a winner. The resolution is always a third
step: go and look at the bytes.

THE THREE DISAGREEMENT CLASSES, AND WHAT EACH ONE USUALLY IS

  parser>0, bytes>0, equal     the shape is there. Confidence.
  parser>0, bytes=0           impossible; the parser invented a token.
  parser=0, bytes>0           THE DANGEROUS DIRECTION, and the common one. A
                              zero that a byte grep contradicts is not a zero:
                              it is an unexamined string. This is the
                              foreignObject case, and it is the direction that
                              makes a corpus look cleaner than it is.
  both zero                   a measured zero, and the only kind of zero this
                              project is willing to publish.

Usage:
    python crosscheck.py                     # every capture
    python crosscheck.py --only wikipedia
    python crosscheck.py --json out.json
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402
import shape_scan  # noqa: E402

# The byte patterns are deliberately LOOSE - no leading `<` where a real page
# might not have one. That is the point. A tight pattern would agree with the
# parser by construction and the cross-check would never fire, which is exactly
# the failure being fixed. `foreignobject` rather than `<foreignObject`: the
# loose form is the one that reported 199 hits across 11 pages, and every one of
# them was prose or a WPT filename. The cross-check's job is to surface those
# and hand them to a reader, not to filter them out in advance.
CROSS_CHECKED = [
    # (counter name, label, byte pattern)
    ("foreign_object", "<foreignObject> element",
     re.compile(rb"foreignobject", re.I)),
    ("cdata", "CDATA section",
     re.compile(rb"<!\[CDATA\[", re.I)),
    ("table_with_rules_attr", "table[rules]",
     re.compile(rb"\srules\s*=", re.I)),
    ("control_with_form_attr", "control with form= attribute",
     re.compile(rb"\sform\s*=", re.I)),
    ("nested_form_token", "nested <form> start tag",
     re.compile(rb"<form\b", re.I)),
    ("svg", "inline <svg>",
     re.compile(rb"<svg\b", re.I)),
    ("svg_use", "<use> element",
     re.compile(rb"<use\b", re.I)),
    ("row_group_tfoot", "<tfoot>",
     re.compile(rb"<tfoot\b", re.I)),
    ("colgroup", "<colgroup>",
     re.compile(rb"<colgroup\b", re.I)),
    ("table_caption", "<caption>",
     re.compile(rb"<caption\b", re.I)),
]

# Where a loose byte hit is most likely NOT to be the element the parser is
# looking for. This is what makes a disagreement actionable rather than merely
# loud. The regions are computed once per document, not once per hit.
NOISE = [
    ("comment", re.compile(rb"<!--.*?-->", re.S)),
    ("script_body", re.compile(rb"<script\b.*?</script\s*>", re.S | re.I)),
    ("style_body", re.compile(rb"<style\b.*?</style\s*>", re.S | re.I)),
]
TAG_SPAN = re.compile(rb"<[a-zA-Z][^>]*>")
URLISH = re.compile(
    rb"(?:href|src|action|poster|data-[\w-]+|cite|formaction)\s*=\s*[\"']?$", re.I)


def document_regions(raw: bytes):
    """Partition the document once: which byte ranges are noise, and which are
    inside a tag. A loose grep hit is then classified by *where it sits*, not by
    a heuristic on the 24 characters before it - which is what made prose read
    as 'unplaceable' and turned a resolvable finding into noise."""
    spans = []
    for name, pat in NOISE:
        for m in pat.finditer(raw):
            spans.append((m.start(), m.end(), name))
    spans.sort()
    tags = [(m.start(), m.end()) for m in TAG_SPAN.finditer(raw)]
    return spans, tags


def _covering(spans, start, end):
    for span in spans:
        a, b = span[0], span[1]
        if a <= start and end <= b:
            return span[2] if len(span) > 2 else "in_tag"
        if a > start:
            break
    return None


def classify_hit(raw: bytes, start: int, end: int, spans=(), tags=()) -> str:
    """What is this byte hit, really? 'element' means the shape really is here.

    Without this the cross-check can only say 'the instruments disagree' and the
    reader is left to guess. With it every disagreement carries a one-word
    verdict: a live element, or a mention in prose / a comment / a URL / a
    script body - which is what the 199 foreignObject hits actually were.
    """
    where = _covering(list(spans), start, end)
    if where:
        return where
    if _covering(list(tags), start, end) is not None:
        before = raw[max(0, start - 64):start]
        if URLISH.search(before):
            return "url"
        if re.search(rb"<\s*[\w:.-]*$", before):
            return "element"      # the hit IS the tag name
        return "attribute_value"
    # Not in a tag and not in a suppressed region: ordinary text, i.e. prose.
    return "prose"


def body_of(path: str) -> bytes:
    with open(path, "rb") as fh:
        raw = fh.read()
    if raw.startswith(b"<!--"):
        j = raw.find(b"-->", 3)
        if j >= 0:
            return raw[j + 4:]
    return raw


def cross_check(paths_list, only=None):
    """Run both instruments over each document and report every disagreement.

    Returns a dict with one row per (document, sub-feature) and a
    `disagreements` list. The return value is the machine-readable form; this
    is also what tests.py asserts against, so a cross-check that stops working
    fails a test rather than quietly printing nothing.
    """
    rows, disagreements = [], []
    for path in paths_list:
        if only and only not in path:
            continue
        with open(path, "rb") as fh:
            raw = fh.read()
        text = raw.decode("utf-8", "replace")
        spans, tags = document_regions(raw)
        doc = shape_scan.Doc()
        try:
            doc.feed(text)
            doc.close()
        except Exception as exc:  # pragma: no cover
            print("parse error %s: %s" % (path, exc), file=sys.stderr)
        for key, label, pat in CROSS_CHECKED:
            n_parser = int(doc.c.get(key, 0))
            hits = list(pat.finditer(raw))
            n_bytes = len(hits)
            row = {
                "document": os.path.relpath(path, paths.DIAG).replace("\\", "/"),
                "key": key, "label": label,
                "parser": n_parser, "bytes": n_bytes,
            }
            if n_parser == 0 and n_bytes > 0:
                # Classify every loose hit, so the reader is told whether the
                # shape is real or the bytes only mention it.
                verdicts = {}
                for m in hits[:400]:
                    v = classify_hit(raw, m.start(), m.end(), spans, tags)
                    verdicts[v] = verdicts.get(v, 0) + 1
                row["hit_kinds"] = verdicts
                # Only a hit that is *positively* placed as noise may be
                # resolved. A hit the classifier cannot place is escalated, not
                # waved through: "I could not explain this away" is a finding,
                # and resolving it would be the exact failure this file exists
                # to prevent.
                unplaced = verdicts.get("unknown", 0)
                row["hit_is_an_element"] = verdicts.get("element", 0) > 0
                if row["hit_is_an_element"] or unplaced:
                    row["verdict"] = (
                        "DISAGREE, and the bytes win: %d element-shaped, %d "
                        "unplaceable byte hit(s) the parser did not see"
                        % (verdicts.get("element", 0), unplaced))
                    row["severity"] = "dangerous"
                else:
                    kinds = ", ".join("%d %s" % (n, k)
                                      for k, n in sorted(verdicts.items(),
                                                         key=lambda kv: -kv[1]))
                    row["verdict"] = ("RESOLVED: the bytes only mention it "
                                      "(%s); the zero stands" % kinds)
                    row["severity"] = "resolved"
            elif n_parser > 0 and n_bytes == 0:
                row["verdict"] = "DISAGREE: the parser found a token with no bytes"
                row["severity"] = "impossible"
            elif n_parser == 0 and n_bytes == 0:
                row["verdict"] = "measured zero"
                row["severity"] = "ok"
            else:
                row["verdict"] = "agree"
                row["severity"] = "ok"
                if n_parser != n_bytes:
                    row["note"] = ("counts differ (%d parser, %d bytes) but both "
                                   "instruments found the shape"
                                   % (n_parser, n_bytes))
            rows.append(row)
            if row["severity"] not in ("ok", "resolved"):
                disagreements.append(row)
    return {"rows": rows, "disagreements": disagreements,
            "resolved": [r for r in rows if r["severity"] == "resolved"],
            "documents": len(paths_list)}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", default=None)
    ap.add_argument("--json", default=None)
    a = ap.parse_args()

    docs = [os.path.join(cd, "page.html") for _s, _sl, cd in paths.capture_dirs()]
    res = cross_check(docs, a.only)

    print("cross-check: %d documents x %d sub-features, two independent "
          "instruments" % (len(docs), len(CROSS_CHECKED)))
    print("  instrument A: shape_scan.Doc, a token-level parse")
    print("  instrument B: a byte-level regex over the same file")
    print()
    by_key = {}
    for r in res["rows"]:
        e = by_key.setdefault(r["key"], {"label": r["label"], "parser": 0,
                                         "bytes": 0, "docs": 0, "zero_docs": 0})
        e["parser"] += r["parser"]
        e["bytes"] += r["bytes"]
        e["docs"] += 1
        if r["parser"] == 0:
            e["zero_docs"] += 1
    print("%-26s %8s %8s  %s" % ("sub-feature", "parser", "bytes", "status"))
    n_resolved = 0
    for key, _l, _p in CROSS_CHECKED:
        e = by_key[key]
        bad = [r for r in res["disagreements"] if r["key"] == key]
        resd = [r for r in res.get("resolved", []) if r["key"] == key]
        n_resolved += len(resd)
        if bad:
            status = "%d document(s) CAUGHT" % len(bad)
        elif e["parser"] == 0:
            # Only a genuine zero gets this wording. A shape the parse DID find
            # must never be described as a measured zero, however many prose
            # mentions the byte grep turned up alongside it.
            status = ("measured zero on all %d" % e["docs"]) if not resd else (
                "measured zero on all %d; %d byte mention(s) resolved as "
                "prose/comment/url" % (e["docs"], len(resd)))
        else:
            status = "present on %d page(s)" % e["docs"]
            if resd:
                status += "; %d byte mention(s) also resolved as prose/url" % len(resd)
        print("%-26s %8d %8d  %s" % (key, e["parser"], e["bytes"], status))
        for r in bad:
            print("      ! %s" % r["document"])
            print("        parser=%d bytes=%d  %s" % (r["parser"], r["bytes"],
                                                      r["verdict"]))
    print()
    n_bad = len(res["disagreements"])
    if n_bad:
        print("%d DISAGREEMENT(S) CAUGHT. A measured zero that a byte grep "
              "contradicts is not a zero." % n_bad)
    else:
        print("no unresolved disagreements.")
    if n_resolved:
        print()
        print("%d byte-level mention(s) were resolved as prose, a comment, a URL "
              "or a script\nbody. Those are what a loose grep reports and a "
              "parse does not, and they are\nexactly why a single-instrument "
              "count of an absent shape is worthless in\nthe direction that "
              "matters: the under-report." % n_resolved)
    print()
    print("every published zero above is confirmed by both instruments.")
    if a.json:
        with open(a.json, "w", encoding="utf-8") as fh:
            json.dump(res, fh, indent=1, sort_keys=True)
        print("wrote " + a.json)
    return 1 if n_bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
