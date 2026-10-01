#!/usr/bin/env python3
"""Per-capture asset manifest: the binaries deliberately left out, by class,
with their URL and content-length. Writes ASSETS.md per capture directory.

No binary is ever fetched. Sizes come from a HEAD request through the same
proxy the capture used.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402

CORPUS = paths.CORPUS
PROXY = paths.PROXY
UA = paths.UA

EXT = {
    "font": re.compile(r"\.(woff2?|ttf|otf|eot)(\?|#|$)", re.I),
    "video": re.compile(r"\.(mp4|webm|m4v|mov|ogv)(\?|#|$)", re.I),
    "audio": re.compile(r"\.(mp3|m4a|ogg|oga|wav|aac|opus)(\?|#|$)", re.I),
    "image": re.compile(r"\.(png|jpe?g|gif|webp|avif|svg|bmp|ico)(\?|#|$)", re.I),
}
# The page's own origin is not a binary; same for API endpoints that return
# JSON, and for the tracking/analytics URLs the stripper already removed.
SKIP = re.compile(r"(google-analytics|googletagmanager|doubleclick|facebook\.net/tr|"
                  r"hotjar|segment\.(io|com)|sentry|newrelic|nr-data|"
                  r"plausible\.io|/w/load\.php|/api/|/rest\.php|\.json\b|"
                  r"beacon|/stats|/log$|\.php$)", re.I)


def kind_of(url):
    for k, pat in EXT.items():
        if pat.search(url):
            return k
    return None


def head(url):
    try:
        p = subprocess.run(
            ["curl.exe", "-s", "-I", "-L", "--max-time", "10", "--proxy", PROXY,
             "-A", UA, url], capture_output=True, timeout=18)
    except subprocess.TimeoutExpired:
        return None
    if p.returncode != 0:
        return None
    txt = p.stdout.decode("utf-8", "replace")
    m = re.findall(r"(?im)^content-length:\s*(\d+)\s*$", txt)
    return int(m[-1]) if m else None


def main():
    with open(paths.data("capture_index.json"), encoding="utf-8") as fh:
        idx = json.load(fh)
    only = sys.argv[1] if len(sys.argv) > 1 else None
    grand = 0
    for r in idx:
        if not r.get("reachable_through_proxy"):
            continue
        shape, slug = r["shape"], r["slug"]
        if only and only not in slug:
            continue
        d = os.path.join(CORPUS, shape, slug)
        page = os.path.join(d, "page.html")
        if not os.path.exists(page):
            continue
        text = open(page, "rb").read().decode("utf-8", "replace")
        by_kind = defaultdict(list)
        for m in re.finditer(r'(?:src|href|poster|data-src|srcset)\s*=\s*["\']([^"\']+)["\']',
                             text, re.I):
            for u in re.split(r"\s*,\s*", m.group(1)):
                u = u.strip().split(" ")[0]
                if not u or u.startswith(("data:", "#", "about:", "javascript:")):
                    continue
                k = kind_of(u)
                if k and not SKIP.search(u):
                    by_kind[k].append(u)
        # @font-face srcs from the captured stylesheets
        for f in sorted(os.listdir(d)):
            if not f.endswith(".css"):
                continue
            ct = open(os.path.join(d, f), "rb").read().decode("utf-8", "replace")
            for m in re.finditer(r"url\(\s*['\"]?([^'\")]+)", ct):
                u = m.group(1).strip()
                if u.startswith(("data:", "about:")):
                    continue
                k = kind_of(u) or ("font" if "@font-face" in ct else None)
                if k and not SKIP.search(u):
                    by_kind[k].append(u)

        lines = ["# Binaries deliberately left out of this capture", "",
                 "No font, image, audio or video byte is present in this "
                 "directory. The documents and stylesheets record the URLs, "
                 "and the sizes below come from a HEAD request through the "
                 "same proxy the capture used.", "",
                 "| class | count | bytes known | largest |", "| --- | ---: | ---: | --- |"]
        tot_n = tot_b = 0
        details = []
        for k in ("font", "image", "video", "audio"):
            urls = sorted(set(by_kind.get(k, [])))
            if not urls:
                continue
            sizes = {}
            for u in urls:
                s = head(u)
                if s:
                    sizes[u] = s
            known = sum(sizes.values())
            big = max(sizes.items(), key=lambda kv: kv[1]) if sizes else (None, 0)
            lines.append("| %s | %d | %s | %s |" % (
                k, len(urls), "{:,}".format(known) if known else "unknown",
                ("%s (%s)" % (big[1] and "{:,}".format(big[1]), big[0].rsplit("/", 1)[-1][:44]))
                if big[0] else "-"))
            tot_n += len(urls)
            tot_b += known
            if k == "font" or len(urls) <= 12:
                for u, s in sorted(sizes.items(), key=lambda kv: -kv[1])[:14]:
                    details.append("| %s | `%s` | %s |" % (
                        k, u, "{:,}".format(s) if s else "unknown"))
        grand += tot_b
        lines += ["", "**%d asset URLs, %s bytes confirmed by HEAD.** Those "
                  "bytes are not in this directory and were never downloaded."
                  % (tot_n, "{:,}".format(tot_b) if tot_b else "an unknown number of"), ""]
        if details:
            lines += ["## Font and small-class URLs", "",
                      "| class | url | bytes |", "| --- | --- | ---: |"] + details + [""]
        else:
            lines += ["## Font and small-class URLs", "",
                      "None recorded. Every asset on this page is a large "
                      "image or media file; the full URL list is in the page "
                      "itself.", ""]
        with open(os.path.join(d, "ASSETS.md"), "wb") as fh:
            fh.write(("\n".join(lines)).encode("utf-8"))
        print("%-16s %-46s %4d urls  %s B" % (shape, slug[:46], tot_n,
                                              "{:,}".format(tot_b)))
    print("\nconfirmed-excluded total across all captures: %s bytes"
          % "{:,}".format(grand))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
