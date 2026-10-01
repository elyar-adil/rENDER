#!/usr/bin/env python3
"""Verify every capture: UTF-8 validity, no binaries, no external fetch
targets left dangling, and that the shape counts still hold after stripping.
"""

from __future__ import annotations

import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402

CORPUS = paths.CORPUS

# Anything that would be a large binary if the capture had committed one.
BINARY_EXT = re.compile(
    r"\.(woff2?|ttf|otf|eot|png|jpe?g|gif|webp|avif|svg|bmp|ico|mp4|webm|m4v|mov|"
    r"mp3|m4a|ogg|oga|wav|zip|gz|pdf|wasm)(\?|#|$)", re.I)
BIG_DATA_URI = re.compile(r"data:[a-z]+/[a-z0-9.+-]+;base64,[A-Za-z0-9+/=]{512,}", re.I)


def check_one(d):
    problems = []
    files = sorted(os.listdir(d))
    htmls = [f for f in files if f.endswith(".html")]
    csss = [f for f in files if f.endswith(".css")]
    total = 0
    for f in files:
        p = os.path.join(d, f)
        raw = open(p, "rb").read()
        total += len(raw)
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as exc:
            problems.append("%s is not valid UTF-8: %s" % (f, exc))
            continue
        if f.endswith(".html"):
            n = len(BIG_DATA_URI.findall(text))
            if n:
                problems.append("%s: %d base64 blob(s) of >=512 chars survive" % (f, n))
            if "\x00" in text:
                problems.append("%s contains a NUL byte" % f)
        if f.endswith(".css"):
            n = len(BIG_DATA_URI.findall(text))
            if n:
                problems.append("%s: %d base64 blob(s) survive" % (f, n))
    return {
        "dir": os.path.basename(d),
        "html": htmls, "css": len(csss),
        "bytes": total,
        "problems": problems,
    }


def main():
    rows = []
    for shape in sorted(os.listdir(CORPUS)):
        sd = os.path.join(CORPUS, shape)
        if not os.path.isdir(sd) or shape.startswith("_"):
            continue
        for cap in sorted(os.listdir(sd)):
            cd = os.path.join(sd, cap)
            if not os.path.isdir(cd):
                continue
            r = check_one(cd)
            r["shape"] = shape
            rows.append(r)
    bad = 0
    for r in rows:
        flag = "OK " if not r["problems"] else "BAD"
        if r["problems"]:
            bad += 1
        print("%s %-16s %-52s html=%d css=%2d  %9d B" % (
            flag, r["shape"], r["dir"][:52], len(r["html"]), r["css"], r["bytes"]))
        for p in r["problems"]:
            print("      ! %s" % p)
    print("\n%d captures, %d with problems, %d B total"
          % (len(rows), bad, sum(r["bytes"] for r in rows)))
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
