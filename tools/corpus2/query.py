#!/usr/bin/env python3
"""Query specific measured counters across a scan file.

    python query.py scan_survey.json nested_form_token form_attr_reaches_outside_form
    python query.py scan_survey.json --min 1 foreign_object cdata
"""
from __future__ import annotations
import json, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402

PROV = paths.data("provenance_survey.json")


def main():
    args = sys.argv[1:]
    path = paths.data("scan_survey.json")
    mn = 1
    if args and args[0].endswith(".json"):
        path = args.pop(0)
    if args and args[0] == "--min":
        mn = int(args.pop(1))
    with open(path, encoding="utf-8") as fh:
        rows = json.load(fh)
    prov = {}
    if os.path.exists(PROV):
        with open(PROV, encoding="utf-8") as fh:
            for r in json.load(fh):
                prov[r["name"]] = r
    keys = args
    hits = 0
    for r in rows:
        c = r["counts"]
        # `>= mn` alone is wrong: with --min 0 a *measured zero* would satisfy
        # it, and the tool would print every key on every page - which reads as
        # universal presence and is the exact opposite of what was measured.
        # A zero is never a hit at any threshold.
        present = {k: c.get(k, 0) for k in keys
                   if c.get(k, 0) > 0 and c.get(k, 0) >= mn}
        if not present:
            continue
        hits += 1
        name = r["file"].replace(".html", "")
        url = prov.get(name, {}).get("url", "?")
        print("%s" % url)
        print("    " + "  ".join("%s=%d" % kv for kv in sorted(present.items())))
    print("\n%d of %d pages matched" % (hits, len(rows)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
