#!/usr/bin/env python3
"""Drive fetch.py over a shape-grouped candidate list, then shape_scan.py
over whatever landed, and print a per-shape ranking so pages are selected by
measurement rather than by reputation.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402

SCRATCH = paths.SCRATCH
os.makedirs(SCRATCH, exist_ok=True)


def parse_candidates(path):
    groups, cur = {}, None
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line or line == "EOF":
                continue
            if " " not in line and "://" not in line:
                cur = line
                groups.setdefault(cur, [])
            else:
                groups.setdefault(cur, []).append(line)
    return groups


def fetch_all(cand_path, prov_path):
    groups = parse_candidates(cand_path)
    seen, args, order = {}, [], []
    for shape, urls in groups.items():
        for u in urls:
            if u in seen:
                continue
            seen[u] = shape
            name = fetch_slug(u)
            args += ["--name", name, u]
            order.append((shape, name, u))
    cmd = [sys.executable, os.path.join(HERE, "fetch.py"), "--out", prov_path] + args
    print("fetching %d urls through the proxy..." % len(order), flush=True)
    p = subprocess.run(cmd)
    return order, p.returncode


def fetch_slug(url):
    return "".join(ch if (ch.isalnum() or ch in "._-") else "_" for ch in url)[:150]


def main():
    cand = sys.argv[1] if len(sys.argv) > 1 else paths.data("candidates.txt")
    prov = paths.data("provenance_survey.json")
    order, rc = fetch_all(cand, prov)

    with open(prov, encoding="utf-8") as fh:
        recs = {r["name"]: r for r in json.load(fh)}

    good = []
    for shape, name, url in order:
        r = recs.get(name, {})
        if r.get("ok") and (r.get("content_type") or "").find("html") >= 0:
            body = os.path.join(SCRATCH, name + ".body")
            dest = os.path.join(SCRATCH, name + ".html")
            if os.path.exists(body) and not os.path.exists(dest):
                with open(body, "rb") as s, open(dest, "wb") as d:
                    d.write(s.read())
            good.append((shape, name, url, r.get("origin_bytes")))

    print("scanning %d html bodies..." % len(good), flush=True)
    scan_in = os.path.join(SCRATCH, "scan_input.txt")
    with open(scan_in, "w", encoding="utf-8") as fh:
        for _s, n, _u, _b in good:
            fh.write(os.path.join(SCRATCH, n + ".html") + "\n")
    p = subprocess.run(
        [sys.executable, os.path.join(HERE, "shape_scan.py"), "--json"] + [scan_in],
        capture_output=True,
    )
    if p.returncode != 0:
        sys.stderr.write(p.stderr.decode("utf-8", "replace"))
    with open(paths.data("scan_survey.json"), "wb") as fh:
        fh.write(p.stdout)

    rows = json.loads(p.stdout.decode("utf-8", "replace"))
    by_shape = {}
    for r in rows:
        for k, v in r["scores"].items():
            by_shape.setdefault(k, []).append((v, r))
    for k in sorted(by_shape):
        print("=" * 78)
        print("SHAPE", k)
        for v, r in sorted(by_shape[k], key=lambda kv: -kv[0])[:6]:
            if v <= 0:
                break
            print("  %5d  %-28s html=%dB" % (v, r["file"][:28], r["html_bytes"]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
