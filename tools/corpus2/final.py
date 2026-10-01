#!/usr/bin/env python3
"""Emit the per-capture tables and totals for the corpus2 README.

Everything printed here is read from capture_index.json, matrix.json and the
capture directories on disk, so the README's numbers are reproducible with
`python _tools/final.py` rather than transcribed by hand.
"""

from __future__ import annotations

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402

CORPUS = paths.CORPUS


def body_of(r):
    """Bytes of the capture excluding its provenance header comment."""
    d = os.path.join(CORPUS, r["shape"], r["slug"])
    p = os.path.join(d, "page.html")
    if not os.path.exists(p):
        return 0
    t = open(p, "rb").read().decode("utf-8", "replace")
    return len(t.split("-->", 1)[1]) if t.startswith("<!--") else len(t)


def human(n):
    if n is None:
        return "-"
    for unit, div in (("MB", 1 << 20), ("KB", 1 << 10)):
        if n >= div:
            return "%d %s" % (n / div, unit)
    return "%d B" % n


def _fidelity():
    """faithful / NORMALISED per capture, from roundtrip.py's own report.

    Read rather than recomputed so the README's verdict and the gate's verdict
    cannot be two different opinions about the same file.
    """
    import roundtrip
    out = {}
    for shape, slug, cd in paths.capture_dirs():
        try:
            d = roundtrip.check_deviations(os.path.join(cd, "page.html"))
            lossless = roundtrip.check_lossless(os.path.join(cd, "page.html"))
        except Exception as exc:  # noqa: BLE001
            out[(shape, slug)] = "check error: %s" % exc
            continue
        if not lossless["ok"]:
            out[(shape, slug)] = "**NORMALISED** (round-trip fails)"
        elif d["parser_visible"]:
            out[(shape, slug)] = "**NORMALISED** (%s)" % ",".join(
                d["parser_visible"])
        else:
            out[(shape, slug)] = "faithful"
    return out


def main():
    with open(paths.data("capture_index.json"), encoding="utf-8") as fh:
        idx = json.load(fh)
    with open(paths.data("matrix.json"), encoding="utf-8") as fh:
        m = json.load(fh)
    counts, npg = {}, {}
    for p in m["pages"]:
        for k, v in p["counts"].items():
            counts[k] = counts.get(k, 0) + v
            npg.setdefault(k, 0)
            if v:
                npg[k] += 1

    print("### Per-capture provenance, sizes, and which agent served it\n")
    print("| shape | origin | served to | fetched | original | on disk | "
          "of origin | dropped | sheets | faithful | assets out |")
    print("| --- | --- | --- | --- | ---: | ---: | ---: | ---: | :-: | :-: | ---: |")
    tot_o = tot_s = tot_co = tot_cs = tot_assets = 0
    fidelity = _fidelity()
    for r in idx:
        d = os.path.join(CORPUS, r["shape"], r["slug"])
        # page.html only. The capture's stylesheets are fetched separately and
        # get their own original/kept row, so adding them here would compare a
        # document against a document-plus-stylesheets.
        page = os.path.join(d, "page.html")
        on_disk = os.path.getsize(page) if os.path.exists(page) else 0
        url = r["url"]
        orig = r.get("origin_bytes")
        tot_o += orig or 0
        tot_s += on_disk
        tot_co += r.get("css_origin_bytes") or 0
        tot_cs += r.get("css_stripped_bytes") or 0
        tot_assets += r.get("asset_count_left_out") or 0
        # The ratio is measured against the page body only. The capture also
        # carries a ~750-byte provenance header, and several pages are already
        # 99% text and table markup, so a capture can measure 100% or slightly
        # over while still having dropped every script, comment and data: URI.
        # The dropped-bytes column is the honest measure; the ratio is only a
        # reminder that these pages had little binary to begin with.
        ratio = ""
        if orig and on_disk:
            ratio = "%.0f%%" % (100.0 * on_disk / orig)
        # Dropped is measured against the capture body, excluding the
        # provenance header the stripper prepends, so a capture that measures
        # 101% of its origin (because the header is larger than what it
        # removed) is not reported as having grown.
        dropped = human(max(0, orig - body_of(r))) if orig else "-"
        # Which user agent was on the wire. This is not trivia: one origin in
        # this corpus answers 403 to a browser-shaped UA and 200 to curl's
        # default, so a capture fetched with the wrong agent is not what a
        # browser receives.
        ua = {"browser-ua": "browser", "curl-default": "**curl**"}.get(
            r.get("proxy_ua_that_answered"), "?")
        verdict = fidelity.get((r["shape"], r["slug"]), "not checked")
        print("| %s | `%s` | %s | %s | %s | %s | %s | %s | %s | %s | %s |" % (
            r["shape"], url, ua, r.get("fetched_at", "-") or "-",
            human(orig), human(on_disk), ratio or "-", dropped,
            "%d/%d" % (r.get("css_sheets_kept") or 0,
                       r.get("css_sheets_linked") or 0),
            verdict, r.get("asset_count_left_out", "-")))
    print()
    print("`served to` is the user agent the request went out with. `**curl**` "
          "marks a\ncapture that is *not* what a browser receives: that origin "
          "403s a\nbrowser-shaped agent and 200s curl's default, so its UA is "
          "a fidelity\nhazard for any claim about what a browser is served.")
    print()
    print("page.html: %s fetched -> %s kept across %d captures"
          % (human(tot_o), human(tot_s), len(idx)))
    print("stylesheets: %s fetched -> %s kept" % (human(tot_co), human(tot_cs)))
    print("binaries: %d asset URLs recorded and left out; see each capture's "
          "ASSETS.md" % tot_assets)
    nf = sum(1 for v in fidelity.values() if v == "faithful")
    print("fidelity: %d of %d captures are byte-faithful to what the origin "
          "served" % (nf, len(fidelity)))

    print("\n### The round-trip result, per capture\n")
    print("`strip.py --lossless` drops nothing and rewrites nothing, so its "
          "output must\nequal its input byte for byte. Run over every capture "
          "body, that is the whole\nclaim: a capture is *faithful* when the "
          "reducer is the identity on it and the\nreduction does nothing "
          "beyond its declared drops.\n")
    print("| capture | lossless identity | declared reductions | stray end tags "
          "| verdict |")
    print("| --- | :-: | --- | ---: | --- |")
    import roundtrip as _rt
    for shape, slug, cd in paths.capture_dirs():
        p = os.path.join(cd, "page.html")
        d = _rt.check_deviations(p)
        lo = _rt.check_lossless(p)
        drops = d["deviation_kinds"]
        print("| %s | %s | %s | %d | %s |" % (
            slug[:48], "PASS" if lo["ok"] else "**FAIL**",
            ", ".join("%s x%d" % (k, v["n"]) for k, v in sorted(drops.items()))
            or "nothing",
            len(d["stray_end_tag_examples"]),
            fidelity.get((shape, slug), "?")))

    print("\n### What each capture was selected for\n")
    for r in idx:
        print("- **%s** - `%s`" % (r["shape"], r["url"]))
        print("  %s" % r.get("why", "-"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
