#!/usr/bin/env python3
"""Is every capture still byte-faithful to what the origin served?

A capture is a *reduced* document, so it is deliberately not equal to the
origin: comments are dropped, external scripts replaced, tracking pixels
removed. The question this answers is narrower and is the one that matters:

    did the reduction do anything to the markup OTHER than its declared
    reductions?

Concretely, did it repair implied end tags, re-quote or re-escape attributes,
un-escape entities, delete a start tag, or rewrite text where whitespace is
significant? Any of those turns real-world markup into normalised markup, and a
parser run over normalised markup measures the parser's handling of a shape it
will never meet, while *not* measuring the recovery path - because the recovery
has already been done for it.

Three levels, cheapest first:

  1. LOSSLESS IDENTITY  (offline, exact, no network)
     strip.py --lossless drops nothing and rewrites nothing, so its output must
     equal its input byte for byte. Run over every capture body. Any difference
     is a defect in the stripper, found before it is baked into a capture.

  2. DEVIATION LEDGER  (offline, exact, no network)
     Run the real reduction over every capture body and enumerate every place
     the output was not a verbatim span of the input, by kind and byte count.
     This is the list a reader needs in order to judge the capture.

  3. ORIGIN FIDELITY  (needs the network)
     Re-fetch each origin and compare the fresh reduction against the capture on
     disk, token by token. Live pages have changed since the capture, so
     differences are classified: CONTENT DRIFT (the page itself moved) is
     expected and is not evidence of anything; a NORMALISATION (an end tag that
     the origin did not have, an attribute rewritten, an entity spelling
     changed) is a defect and is reported by name.

Usage:
    python roundtrip.py            # levels 1 and 2 over every capture
    python roundtrip.py --origin   # all three; re-fetches
    python roundtrip.py --origin --only wikipedia
    python roundtrip.py --json out.json
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from html.parser import HTMLParser

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402
import strip as stripper  # noqa: E402

# Deviations that change what a parser sees, as opposed to deviations that only
# remove bytes. A capture containing any of the first group is NORMALISED and
# says so in the README; a capture containing only the second group is faithful.
PARSER_VISIBLE = {
    "synthesised_end_tag",
    "rewritten_tag_text",
    "rescanned_attribute",
    "entity_respelled",
    "deleted_start_tag",
    "significant_whitespace_collapsed",
}


def header_end(text: str) -> int:
    """Index just past the provenance header comment a capture opens with."""
    if not text.startswith("<!--"):
        return 0
    j = text.find("-->", 3)
    return (j + 3 + 1) if j >= 0 else 0


def body_of(path: str) -> str:
    with open(path, "rb") as fh:
        t = fh.read().decode("utf-8", "replace")
    i = header_end(t)
    return t[i:] if i else t


# --------------------------------------------------------------- token model
class _TagWalker(HTMLParser):
    """Start and end tags, plus reference spellings, from a real tokeniser.

    A hand-written regex for this looked reasonable and was wrong: it failed to
    match `<script data-stripped="external-script" data-origin-url="https://...">`
    and so reported all 75 of the captures' external-script placeholders as
    stray closing tags - which would have condemned 18 byte-faithful captures.
    The reference for "is this a tag" is a tokeniser, not a pattern.

    `convert_charrefs=False` so `&amp;` reaches handle_entityref and its exact
    source spelling can be read, which is what makes a respelled reference
    detectable at all.
    """

    def __init__(self, text):
        super().__init__(convert_charrefs=False)
        self.toks = []
        self._text = text
        self._ls = stripper.line_starts(text)
        self.feed(text)
        self.close()

    def _abs(self):
        line, col = self.getpos()
        if 1 <= line <= len(self._ls):
            return min(self._ls[line - 1] + col, len(self._text))
        return len(self._text)

    def handle_starttag(self, tag, attrs):
        self.toks.append(("start", "<%s>" % tag.lower()))

    def handle_startendtag(self, tag, attrs):
        self.toks.append(("open", "<%s/>" % tag.lower()))

    def handle_endtag(self, tag):
        self.toks.append(("end", "</%s>" % tag.lower()))

    def handle_entityref(self, name):
        i = self._abs()
        m = stripper.REF.match(self._text, i)
        self.toks.append(("ref", m.group(0) if m else "&%s;" % name))

    def handle_charref(self, name):
        i = self._abs()
        m = stripper.REF.match(self._text, i)
        self.toks.append(("ref", m.group(0) if m else "&#%s;" % name))

    def handle_decl(self, decl):
        self.toks.append(("decl", "<!%s>" % decl))

    def handle_comment(self, data):
        i = self._abs()
        src = (self._text[i:i + 4] == "<!--" and
               self._text.find("-->", i) >= 0)
        self.toks.append(("comment", "<!-->" if src else "<!--"))

    def unknown_decl(self, data):
        self.toks.append(("marked", "<![%s]>" % data[:12]))


def tokens(text: str):
    """A flat, comparable token list: tags, references, and declarations.

    Text runs are deliberately absent. The corpus is a live page whose content
    changes between fetches, and a character-level text diff would report
    content drift as if it were damage. What must match exactly is the tag
    structure, the reference spellings, and the declaration tokens.
    """
    return _TagWalker(text).toks


def diff_tokens(a, b):
    """Added/removed token counts by (kind, normalised form)."""
    from collections import Counter
    ca, cb = Counter(a), Counter(b)
    added, removed = [], []
    for k, n in (cb - ca).items():
        added.append((k, n))
    for k, n in (ca - cb).items():
        removed.append((k, n))
    added.sort(key=lambda kv: -kv[1])
    removed.sort(key=lambda kv: -kv[1])
    return added, removed


def norm_tag(v: str) -> str:
    """A tag reduced to its name and attribute *names*, so that a re-quoted or
    re-ordered value does not hide a structural change and vice versa."""
    m = re.match(r"</?\s*([a-zA-Z][^\s/>]*)", v)
    return (m.group(1).lower() if m else v).lower()


def classify_added(kind: str, v: str) -> list[str]:
    """What kind of normalisation does an added token represent?"""
    out = []
    if kind in ("end", "endjunk"):
        out.append("synthesised_end_tag")
    if kind in ("start", "open") and norm_tag(v) == "script":
        out.append("deleted_start_tag")
    if kind == "ref":
        out.append("entity_respelled")
    if kind in ("start", "open", "end", "endjunk"):
        out.append("rewritten_tag_text")
    return out


def classify_tokens(toks):
    """End tags with nothing open - a property of the document, not a verdict.

    A `</table>` closing an open `<td>` is not evidence of anything: that is the
    tree builder's implied end tag, written by every real page, and treating it
    as damage would flag all eighteen captures. A `</td>` with nothing open is
    different - no hand-written page emits one, and it is the exact signature of
    a reducer that emitted a closer for every element it thought it had opened.
    That is what put 957 closers on the Wikipedia page.

    IMPORTANT: finding one here is NOT proof of normalisation, because a real
    page can contain one too. It is a *prompt to check*. The verdict is made by
    comparing against the origin (level 3); the lossless identity (level 1) is
    what actually proves the reducer altered the document. On a corpus where
    level 1 passes and the reducer is a source-span passthrough, every stray end
    tag here is the ORIGIN'S OWN - which is a fidelity positive: it is the
    misnesting the corpus exists to hold.
    """
    out = []
    stack = []
    for kind, v in toks:
        name = norm_tag(v)
        if kind == "start" and name not in VOID_ELEMENTS:
            stack.append(name)
        elif kind in ("end", "endjunk"):
            if name in stack:
                i = len(stack) - 1 - stack[::-1].index(name)
                del stack[i:]
            else:
                out.append((kind, v))
    # Anything still open at EOF is left open, as served, and is not a defect.
    return out


VOID_ELEMENTS = {
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link",
    "meta", "param", "source", "track", "wbr",
}


# ------------------------------------------------------------------ the check
def check_lossless(path: str) -> dict:
    body = body_of(path)
    try:
        s = stripper.strip_html(body, lossless=True)
        return {"ok": True, "bytes": len(body)}
    except AssertionError as exc:
        return {"ok": False, "error": str(exc)}


def check_deviations(path: str, origin_bytes: int | None = None) -> dict:
    body = body_of(path)
    # Self-contained normalisation check, no origin needed. A document that
    # closes an element it never opened has been repaired, whatever produced
    # it, and that is detectable from the file alone.
    implied = classify_tokens(tokens(body))
    s = stripper.strip_html(body)
    by_kind = {}
    for d in s.deviations:
        k = d["kind"]
        e = by_kind.setdefault(k, {"n": 0, "bytes": 0})
        e["n"] += 1
        e["bytes"] += d["bytes"]
    # Whitespace collapse is only significant outside <pre>, <textarea> and an
    # inline white-space:pre* element, and the stripper already exempts those.
    # A class-based white-space:pre is invisible to a tokeniser; count the
    # declarations that exist so the limit is a number and not a shrug.
    ws_classes = len(re.findall(
        r"white-space\s*:\s*pre\b", body, re.I))
    norm_kinds = sorted({k for kind, _v in implied
                         for k in classify_added(kind, _v)})
    # A stray end tag is a prompt to check, not a verdict. Whether it is
    # normalisation depends on whether the reducer altered the document at all,
    # and level 1 answers that. So the parser-visible set here is only the
    # deviations THIS reduction introduces.
    dev_visible = sorted({k for k in by_kind if k in PARSER_VISIBLE})
    return {
        "capture_bytes": len(body.encode("utf-8", "replace")),
        "origin_bytes": origin_bytes,
        "deviation_kinds": by_kind,
        "parser_visible": dev_visible,
        "stray_end_tags_preserved": len(implied),
        "stray_end_tag_examples": ["%s %s" % (k, v) for k, v in implied[:6]],
        "ws_pre_elements_exempted": s.ws_pre_elements,
        "ws_pre_declarations_present": ws_classes,
        "refs_without_semicolon_preserved": s.refs_without_semicolon,
        "comments_removed": s.comments_removed,
        "external_scripts_placeholdered": s.drops.get("external_script_elements", 0),
        "tracking_pixels_removed": s.tracking_removed,
        "implied_end_tags_preserved": s.drops.get("implied_end_tags_preserved", 0),
        "elements_left_open_at_eof": s.drops.get("elements_left_open_at_eof", 0),
        "stray_end_tags": s.drops.get("stray_end_tag", 0),
        "assets_recorded_not_fetched": len(s.assets),
    }


def provenance() -> dict:
    p = paths.data("capture_index.json")
    out = {}
    if not os.path.exists(p):
        return out
    with open(p, encoding="utf-8") as fh:
        for r in json.load(fh):
            out[(r["shape"], r["slug"])] = r
    return out


# -------------------------------------------------------------- level 3: live
def refetch(url: str, want: str) -> bytes | None:
    """Fetch with the same policy the capture used: browser UA first, then curl's
    default, because one origin in this corpus 403s a browser-shaped UA."""
    import subprocess
    os.makedirs(paths.SCRATCH, exist_ok=True)
    dest = os.path.join(paths.SCRATCH, "rt_" + want + ".body")
    for label, ua in (("browser-ua", paths.UA), ("curl-default", None)):
        cmd = ["curl.exe", "-sS", "--compressed", "-L", "--max-time", "60",
               "--max-filesize", str(96 * 1024 * 1024), "--proxy", paths.PROXY,
               "-o", dest, "-w", "%{http_code}"]
        if ua:
            cmd += ["-A", ua]
        cmd.append(url)
        try:
            p = subprocess.run(cmd, capture_output=True, timeout=90)
        except subprocess.TimeoutExpired:
            return None
        code = p.stdout.decode("utf-8", "replace").strip()
        if p.returncode == 0 and code.startswith("2"):
            with open(dest, "rb") as fh:
                return fh.read()
        if p.returncode != 0:
            return None
    return None


def check_origin(shape: str, slug: str, rec: dict) -> dict:
    url = rec.get("url")
    if not url:
        return {"ok": False, "why": "no url recorded"}
    want = re.sub(r"[^A-Za-z0-9._-]", "_", url)[:100]
    raw = refetch(url, want)
    if raw is None:
        return {"ok": False, "why": "origin unreachable now"}
    fresh = raw.decode("utf-8", "replace")
    # 1. the stripper must be the identity on the origin itself
    try:
        stripper.strip_html(fresh, lossless=True)
        lossless_ok = True
        lossless_why = ""
    except AssertionError as exc:
        lossless_ok = False
        lossless_why = str(exc)
    # 2. the reduction of the origin, against the reduction on disk
    a = tokens("".join(stripper.strip_html(fresh).out))
    b = tokens(body_of(os.path.join(paths.CORPUS, shape, slug, "page.html")))
    added, removed = diff_tokens(a, b)
    normalisation = []
    for kind, n in added:
        normalisation += [(k, n) for k in classify_added(kind, "<x>")]
    # Which *specific* end tags appear on disk that the origin lacks?
    from collections import Counter
    ca = Counter(a)
    cb = Counter(b)
    only_on_disk = cb - ca
    invented = [(k[1], n) for k, n in only_on_disk.items() if k[0] in ("end", "endjunk")]
    respelled = [(k[1], n) for k, n in only_on_disk.items() if k[0] == "ref"]
    return {
        "ok": True,
        "origin_bytes": len(raw),
        "lossless_identity_on_origin": lossless_ok,
        "lossless_error": lossless_why,
        "tokens_origin": len(a),
        "tokens_capture": len(b),
        "tokens_only_on_capture": sum(n for _k, n in added),
        "tokens_only_on_origin": sum(n for _k, n in removed),
        "end_tags_on_capture_not_in_origin": sorted(invented, key=lambda x: -x[1])[:12],
        "refs_on_capture_not_in_origin": sorted(respelled, key=lambda x: -x[1])[:12],
        "content_drift_only": (
            not invented and not respelled
            and sum(n for _k, n in removed) == 0),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--origin", action="store_true",
                    help="also re-fetch each origin (level 3, needs network)")
    ap.add_argument("--only", default=None, help="substring match on the slug")
    ap.add_argument("--json", default=None, help="write the report as JSON")
    a = ap.parse_args()

    prov = provenance()
    rows = []
    bad_lossless = 0
    normalised = 0
    for shape, slug, cd in paths.capture_dirs():
        if a.only and a.only not in slug:
            continue
        page = os.path.join(cd, "page.html")
        rec = prov.get((shape, slug), {})
        row = {
            "shape": shape, "slug": slug,
            "url": rec.get("url"),
            "origin_bytes_recorded": rec.get("origin_bytes"),
            "ua_that_answered": rec.get("proxy_ua_that_answered"),
            "fetched_at": rec.get("fetched_at"),
        }
        row["level1_lossless_identity"] = check_lossless(page)
        if not row["level1_lossless_identity"]["ok"]:
            bad_lossless += 1
        row["level2_deviations"] = check_deviations(
            page, rec.get("origin_bytes"))
        if row["level2_deviations"]["parser_visible"]:
            normalised += 1
            row["verdict"] = "NORMALISED"
        else:
            row["verdict"] = "faithful"
        if a.origin:
            row["level3_origin_fidelity"] = check_origin(shape, slug, rec)
        rows.append(row)

    hdr = "lossless-identity  deviations  verdict      capture"
    print("=" * len(hdr))
    print(hdr)
    print("=" * len(hdr))
    for r in rows:
        l1 = "PASS" if r["level1_lossless_identity"]["ok"] else "FAIL"
        d = r["level2_deviations"]
        print("%-5s %-18s %-7s %-12s %9d B  %s" % (
            l1, r["slug"][:18],
            (",".join("%s:%d" % (k, v["n"]) for k, v in
                      sorted(d["deviation_kinds"].items()))) or "none",
            r["verdict"], d["capture_bytes"], (r["url"] or "")[:40]))
        if not r["level1_lossless_identity"]["ok"]:
            print("      ! %s" % r["level1_lossless_identity"]["error"][:200])
        if a.origin and r.get("level3_origin_fidelity"):
            lo = r["level3_origin_fidelity"]
            if not lo.get("ok"):
                print("      - level 3: %s" % lo.get("why"))
            else:
                print("      level 3: origin %d B, tokens %d vs %d, "
                      "lossless-on-origin=%s" % (
                          lo["origin_bytes"], lo["tokens_origin"],
                          lo["tokens_capture"],
                          lo["lossless_identity_on_origin"]))
                if lo["end_tags_on_capture_not_in_origin"]:
                    print("      ! NORMALISED: end tags on the capture that the "
                          "origin does not have: %s" % (
                              ", ".join("%s x%d" % (t, n) for t, n in
                                        lo["end_tags_on_capture_not_in_origin"])))
                if lo["refs_on_capture_not_in_origin"]:
                    print("      ! respelled references: %s" % (
                        ", ".join("%s x%d" % (t, n) for t, n in
                                  lo["refs_on_capture_not_in_origin"])))

    print()
    print("%d captures: %d lossless-identity failures, %d NORMALISED, "
          "%d faithful" % (len(rows), bad_lossless, normalised,
                           len(rows) - normalised))
    if a.json:
        with open(a.json, "w", encoding="utf-8") as fh:
            json.dump({"rows": rows}, fh, indent=1, sort_keys=True)
        print("wrote " + a.json)
    return 1 if bad_lossless or normalised else 0


if __name__ == "__main__":
    raise SystemExit(main())
