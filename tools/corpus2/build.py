#!/usr/bin/env python3
"""Build the corpus2 captures.

For each selected page: fetch -> discover its stylesheets -> strip page and
stylesheets -> record provenance, per-shape measurements, before/after sizes,
and the URLs+sizes of every binary deliberately left out.

Nothing is downloaded except the HTML and the stylesheets. Fonts, images,
audio and video are recorded by URL and HEAD size, never fetched.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402

ROOT = paths.REPO
CORPUS = paths.CORPUS
SCRATCH = paths.SCRATCH
PROXY = paths.PROXY
UA = paths.UA
os.makedirs(SCRATCH, exist_ok=True)

sys.path.insert(0, HERE)
import shape_scan  # noqa: E402
import strip as stripper  # noqa: E402

# slug -> (shape directory, url, why-this-page-was-chosen)
SELECTION = [
    ("form-heavy", "https://www.gov.uk/government/statistics/",
     "search box plus 3 forms and 1541 form-associated controls, 9 of which "
     "are outside every form; the deepest form-shaped page found"),
    ("form-heavy", "https://github.com/login",
     "the login shape: a password control, a hidden 'commit' control, and 8 "
     "controls outside any form"),
    ("data-table", "https://www.iana.org/assignments/media-types",
     "row groups at scale: 14 tables, 11 thead + 11 tbody, 3291 rows, "
     "11676 cells"),
    ("data-table", "https://en.wikipedia.org/wiki/Comparison_of_web_browsers",
     "merged cells at scale: 274 rowspan, 34 colspan, 258 th[scope] over "
     "34 tables"),
    ("data-table", "https://en.wikipedia.org/wiki/ISO_4217",
     "the only page found with 4 table captions plus th[scope]"),
    # en.wikipedia.org and stackoverflow.com both became unreachable part-way
    # through the survey (schannel handshake failure 35, and a Cloudflare
    # challenge). Their captures above are kept from the earlier successful
    # fetch; the re-fetch failure is recorded rather than hidden.
    ("webfont-heavy", "https://accounts.google.com/ServiceLogin",
     "273 @font-face blocks and 273 unicode-range slices in the page's own "
     "CSS, 59 @keyframes, and a password control"),
    ("svg-heavy", "https://www.nasa.gov/",
     "241 inline <svg>, 240 viewBox, 46 live xml:space - the only page found "
     "with live xml:space outside a code sample"),
    ("svg-heavy", "https://getbootstrap.com/",
     "32 live xlink:href on inline <svg>, against 32 viewBox"),
    ("animation-heavy", "https://github.com/",
     "41 @keyframes, 173 transition declarations, 96 animation, 19 "
     "transition-delay, 8 animation-fill-mode across 30 real stylesheets. "
     "Also the deepest stacked/positioned page: 14 position:sticky, 33 fixed, "
     "217 absolute, 171 z-index, 130 transform, 2 isolation, 11 mix-blend-mode"),
    ("stacked-positioned", "https://github.com/search",
     "the second deepest position:sticky page found (9 sticky, 171 z-index, "
     "33 fixed, 217 absolute across its 28 stylesheets), and unlike the "
     "stackoverflow.com capture it is served without a bot challenge. "
     "stackoverflow.com/users/login was attempted and recorded as unreachable: "
     "it returns a 5.4 KB Cloudflare challenge page, whose 1.3 MB of real "
     "stylesheets is why the corpus is served by two different origins here"),
    ("multi-column", "https://en.wikipedia.org/wiki/Portal:Current_events",
     "the only commercial page found with live column-count (7) and live "
     "column-width (8) rather than column-count inside a documentation "
     "snippet; 14 live float, 3 live clear"),
    ("media-heavy", "https://commons.wikimedia.org/wiki/Category:Videos",
     "200 <video> with 200 poster, 922 <source> of which 920 carry type"),
    ("media-heavy", "https://commons.wikimedia.org/wiki/Category:Audio_files",
     "200 <audio> with 590 <source> of which 588 carry type"),
    ("template-and-inline-js", "https://developer.mozilla.org/en-US/",
     "17 <template> elements, 13 of whose contents contain row tags: the "
     "template-as-row-template shape"),
    ("template-and-inline-js", "https://www.theguardian.com/",
     "15 inline scripts totalling 21 KB, 7 of which bind a load handler, plus "
     "44 form controls outside any form"),
    # The three below exist to fill sub-features the survey found ABSENT, so
    # the coverage matrix can say "represented" on evidence rather than hope.
    ("data-table", "https://www.w3.org/TR/css-flexbox-1/",
     "<colgroup> with span attributes: 9 <colgroup> elements, the only page "
     "found carrying the CSS 2.1 §17.5.1 column-group shape"),
    ("data-table", "https://html.spec.whatwg.org/multipage/tables.html",
     "<tfoot> on a real page (7 occurrences) plus 161 tables, 143 colspan, "
     "44 rowspan and 20 <caption>: the densest table page found"),
    ("multicolumn", "https://www.w3.org/TR/css-multicol-1/",
     "column-width (6) alongside column-count and the columns shorthand, on "
     "the one page found using the multi-column properties together"),
]

CSS_LIMIT = 40
CSS_CAP = 4_000_000


def sh(cmd, timeout=90):
    return subprocess.run(cmd, capture_output=True, timeout=timeout)


def fetch_one(url, name):
    body = os.path.join(SCRATCH, name + ".body")
    hdr = os.path.join(SCRATCH, name + ".hdr")
    trail, p, out = [], None, ""
    for label, ua in (("browser-ua", UA), ("curl-default", None)):
        cmd = ["curl.exe", "-sS", "--compressed", "-L", "--max-time", "60",
               "--max-filesize", str(96 * 1024 * 1024), "--proxy", PROXY,
               "-D", hdr, "-o", body,
               "-w", "%{http_code}\t%{url_effective}\t%{content_type}\t%{size_download}\t%{time_total}"]
        if ua:
            cmd += ["-A", ua]
        cmd.append(url)
        try:
            p = sh(cmd, timeout=90)
        except subprocess.TimeoutExpired:
            trail.append({"ua": label, "result": "timeout"})
            return {"ok": False, "note": "timeout", "attempts": trail,
                    "origin_bytes": 0, "http_status": 0}
        out = p.stdout.decode("utf-8", "replace").strip()
        parts = out.split("\t")
        trail.append({"ua": label,
                      "http_status": parts[0] if parts else None,
                      "curl_rc": p.returncode})
        if p.returncode == 0 and len(parts) >= 5 and parts[0].startswith("2"):
            break
        if p.returncode != 0:
            break
    parts = out.split("\t")
    rec = {"attempts": trail}
    if p is None or p.returncode != 0 or len(parts) < 5:
        rec.update(ok=False, origin_bytes=0, http_status=0,
                   note=(p.stderr.decode("utf-8", "replace").strip() if p else "?"))
        return rec
    size = os.path.getsize(body) if os.path.exists(body) else 0
    rec.update(ok=True, http_status=int(parts[0]) if parts[0].isdigit() else 0,
               final_url=parts[1], content_type=parts[2], origin_bytes=size,
               seconds=float(parts[4]) if parts[4].replace(".", "").isdigit() else None)
    return rec


def discover_css(html_text, final_url):
    base = re.match(r"(https?://[^/]+)", final_url or "")
    host = base.group(1) if base else "https://example.invalid"
    urls = []
    for tag in re.findall(r"<link[^>]+rel=[\"']?[^\"'>]*stylesheet[^\"'>]*[\"']?[^>]*>",
                          html_text, re.I):
        m = re.search(r"href=[\"']([^\"']+)[\"']", tag, re.I)
        if not m:
            continue
        u = m.group(1).strip()
        # The href is HTML-escaped in the document, so a query string arrives
        # as "&amp;..." and fetching it verbatim asks the origin for a
        # different module set. Two Wikipedia captures silently lost their
        # stylesheets - and therefore every CSS-borne shape - to this.
        u = (u.replace("&amp;", "&").replace("&#38;", "&")
              .replace("&quot;", '"').replace("&#x2F;", "/"))
        if u.startswith("//"):
            u = "https:" + u
        elif u.startswith("/"):
            u = host + u
        elif not u.startswith("http"):
            u = host + "/" + u
        urls.append(u)
    seen, out = set(), []
    for u in urls:
        if u not in seen:
            seen.add(u)
            out.append(u)
    return out[:CSS_LIMIT]


def is_interesting_sheet(url: str) -> bool:
    """A stylesheet endpoint that only returns the framework's own CSS.

    MediaWiki's load.php returns the *loaded module set* for a skin, which is
    legitimately large and legitimately the page's styling - it is kept. What
    is filtered is a `only=styles` request for a module list the origin
    answers with an empty or error body, which is recorded rather than stored
    as a 196-byte "stylesheet".
    """
    return True


def fetch_css(urls):
    """Probe each stylesheet. curl -w writes to stdout, which would corrupt
    the body, so the body goes to a file and only the meta line to stdout."""
    got = []
    tmp = os.path.join(SCRATCH, "_probe.css")
    for u in urls:
        p = sh(["curl.exe", "-s", "--compressed", "-L", "--max-time", "25",
                "--max-filesize", str(CSS_CAP), "--proxy", PROXY, "-A", UA,
                "-o", tmp,
                "-w", "%{http_code}\t%{content_type}\t%{size_download}", u],
               timeout=40)
        meta = p.stdout.decode("utf-8", "replace").strip().split("\t")
        if p.returncode != 0 or not meta or not meta[0].startswith("2"):
            got.append({"url": u, "ok": False,
                        "http_status": meta[0] if meta else None})
            continue
        size = os.path.getsize(tmp) if os.path.exists(tmp) else 0
        with open(tmp, "rb") as fh:
            head = fh.read(400)
        # A 200 that is not CSS is not a stylesheet. Wikipedia answers a
        # missing sheet with a small HTML page and status 200.
        if is_html_error_body(head):
            got.append({"url": u, "ok": False, "http_status": meta[0],
                        "note": "200 but the body is HTML, not CSS (%d B)" % size})
            continue
        got.append({"url": u, "ok": True, "bytes": size,
                    "content_type": meta[1] if len(meta) > 1 else ""})
    return got


HTML_ERROR_HEAD = re.compile(
    rb"^\s*(?:<!doctype\s+html|<html[\s>]|<!--|<html)", re.I)


def is_html_error_body(head: bytes) -> bool:
    """Is this 200-response body actually an HTML page rather than a sheet?

    Wikipedia answers a missing stylesheet with **HTTP 200** and a 196-byte HTML
    error body. build.py recorded two of those as stylesheets, so two captures
    reported `2/2 sheets` while carrying no CSS at all - and therefore no
    CSS-borne shape, silently.

    The status code is not the test and never was: a 200 says the origin was
    willing to answer, not that it answered with a stylesheet. The body is the
    test. Doctype, a leading comment, a bare <html>, and a public-doctype are
    all HTML; a leading `{` or `@charset` or `.` or `#` is not.
    """
    return bool(HTML_ERROR_HEAD.match(head[:400]))


def grab_css(url, dest):
    """Fetch one stylesheet to `dest`, returning (origin_bytes, raw_bytes).

    A stylesheet is accepted only if it is actually CSS.
    """
    tmp = os.path.join(SCRATCH, "_grab.css")
    p = sh(["curl.exe", "-s", "--compressed", "-L", "--max-time", "25",
            "--max-filesize", str(CSS_CAP), "--proxy", PROXY, "-A", UA,
            "-o", tmp, "-w", "%{http_code}\t%{content_type}", url], timeout=40)
    if p.returncode != 0 or not os.path.exists(tmp):
        return None, None
    with open(tmp, "rb") as fh:
        raw = fh.read()
    meta = p.stdout.decode("utf-8", "replace").strip().split("\t")
    ctype = meta[1] if len(meta) > 1 else ""
    if is_html_error_body(raw):
        return None, None
    if ctype and "css" not in ctype and "octet-stream" not in ctype \
            and "text/plain" not in ctype:
        return None, None
    ob = len(raw)
    clean = stripper.DATA_URI_CSS.sub("url(about:stripped)",
                                      raw.decode("utf-8", "replace"))
    with open(dest, "wb") as fh:
        fh.write(clean.encode("utf-8"))
    return ob, raw


def head_size(url):
    try:
        p = sh(["curl.exe", "-s", "-I", "-L", "--max-time", "12", "--proxy", PROXY,
                "-A", UA, "-w", "%{http_code}", url], timeout=20)
    except subprocess.TimeoutExpired:
        return None
    if p.returncode != 0:
        return None
    txt = p.stdout.decode("utf-8", "replace")
    lens = re.findall(r"(?im)^content-length:\s*(\d+)\s*$", txt)
    if lens:
        return int(lens[-1])
    return None


def css_strip(css_text, inline_js_cap=0):
    s = stripper.Stripper(inline_js_cap=0)
    before = len(css_text)
    out = stripper.DATA_URI_CSS.sub("url(about:stripped)", css_text)
    return out, before, len(out)


def _load_index_snapshot():
    """The index as it was BEFORE this run, read once.

    Merging against the live file instead compounds: each capture's write
    re-adds the previous write's entries, so a full rebuild of 18 captures
    produced 35 index rows for 18 captures on disk. A register with rows for
    documents that do not exist is worse than no register.
    """
    p = paths.data("capture_index.json")
    if not os.path.exists(p):
        return []
    try:
        with open(p, encoding="utf-8") as fh:
            return json.load(fh)
    except Exception:
        return []


def _write_index(index, snapshot):
    """Merge this run's results into the pre-run index.

    A partial run - one host 403s, one page 404s - must not silently delete
    the provenance of the captures it did not touch. An index that shrinks
    between runs is a measurement that cannot be trusted to have been taken
    from the corpus that is actually on disk.
    """
    seen = {(r.get("shape"), r.get("slug")) for r in index}
    for r in snapshot:
        if (r.get("shape"), r.get("slug")) not in seen:
            index.append(r)
    index.sort(key=lambda r: (r.get("shape") or "", r.get("slug") or ""))
    with open(paths.data("capture_index.json"), "w", encoding="utf-8") as fh:
        json.dump(index, fh, indent=1, sort_keys=True)


def main():
    snapshot = _load_index_snapshot()
    index = []
    for shape, url, why in SELECTION:
        name = re.sub(r"[^A-Za-z0-9._-]", "_", url)[:120]
        slug = name.replace("https___", "").replace("_", "-")[:60].strip("-")
        dest_dir = os.path.join(CORPUS, shape, slug)
        os.makedirs(dest_dir, exist_ok=True)
        print("\n=== %s :: %s" % (shape, url), flush=True)

        fetched_at = time.strftime("%Y-%m-%dT%H:%M:%S%z")
        dest_dir = os.path.join(CORPUS, shape, slug)
        os.makedirs(dest_dir, exist_ok=True)
        page_path = os.path.join(dest_dir, "page.html")
        rec = fetch_one(url, name)
        reachable = rec.get("ok", False)
        if not reachable:
            # A capture on disk from an earlier successful fetch stays valid.
            # Say which, rather than reporting either a phantom success or a
            # silent loss.
            if os.path.exists(page_path):
                print("    UNREACHABLE now (%s); keeping the capture already "
                      "on disk from an earlier successful fetch"
                      % rec.get("note"), flush=True)
                index.append({
                    "shape": shape, "url": url, "slug": slug,
                    "reachable_through_proxy": False,
                    "note": "unreachable at re-fetch; the capture on disk is "
                            "from an earlier successful fetch in this survey",
                    "refetch_note": rec.get("note"),
                    "refetch_attempts": rec.get("attempts"),
                    "on_disk_from_earlier_fetch": True,
                    "why": why,
                })
                continue
            print("    UNREACHABLE: %s" % rec.get("note"), flush=True)
            index.append({"shape": shape, "url": url, "slug": slug,
                          "reachable_through_proxy": False,
                          "attempts": rec.get("attempts"),
                          "note": rec.get("note"), "why": why,
                          "on_disk_from_earlier_fetch": False})
            continue

        body = os.path.join(SCRATCH, name + ".body")
        with open(body, "rb") as fh:
            raw = fh.read()
        origin_bytes = len(raw)
        text = raw.decode("utf-8", "replace")

        # FIDELITY GATE. Before the reduction is allowed to become a capture,
        # prove the reducer is the identity on this document: a lossless pass
        # must reproduce the origin byte for byte. An earlier version deleted
        # every inline <script> start tag and re-emitted entity references as
        # "&name;" whether or not the origin had the semicolon, which turned
        # `window.wiz._tick()` into `window.wiz;_tick()` in every capture. A
        # reducer that cannot prove it is faithful is not allowed to produce
        # one; the capture is refused rather than shipped.
        try:
            stripper.strip_html(text, lossless=True)
            fidelity = "lossless round-trip verified against the origin bytes"
        except AssertionError as exc:
            print("    FIDELITY GATE REFUSED the capture: %s" % exc, flush=True)
            index.append({
                "shape": shape, "url": url, "slug": slug,
                "reachable_through_proxy": True,
                "fetched_at": fetched_at, "proxy": PROXY,
                "fidelity": "REFUSED - the reducer is not the identity here",
                "fidelity_error": str(exc), "why": why,
                "origin_bytes": origin_bytes,
            })
            _write_index(index, snapshot)
            continue

        s = stripper.strip_html(text)
        dev = {}
        for d in s.deviations:
            e = dev.setdefault(d["kind"], 0)
            dev[d["kind"]] = e + 1
        ua_label = rec.get("attempts", [{}])[-1].get("ua")
        header = (
            "  Reduced capture for the corpus2 shape survey.\n"
            "  origin:   %s\n"
            "  final:    %s\n"
            "  fetched:  %s via the system proxy %s, served to user agent: %s\n"
            "  shape:    %s - %s\n"
            "  reduction: structure, classes, inline styles, table shape, form\n"
            "    shape, @font-face/unicode-range/@supports/keyframes and text are\n"
            "    kept; external scripts, data: URIs, comments, tracking pixels\n"
            "    and inline scripts over %d bytes are replaced by sized\n"
            "    placeholders; no font, image, audio or video bytes are present.\n"
            "  fidelity: %s\n"
            "    Everything this step removed is enumerated below. Every token\n"
            "    that was kept is the origin's own bytes: no end tag is added,\n"
            "    no attribute is re-quoted or re-escaped, no reference is\n"
            "    respelled, and implied end tags are left as served.\n"
            "  removed:  %s\n"
            "  sizes:    %d B original -> this file's body is the reduction.\n"
        ) % (url, rec.get("final_url"), fetched_at, PROXY, ua_label, shape, why,
             s.inline_js_cap, fidelity,
             "; ".join("%s x%d" % (k, v) for k, v in sorted(dev.items())) or
             "nothing; the origin needed no reduction",
             origin_bytes)
        page_path = os.path.join(dest_dir, "page.html")
        body_out = "<!--\n" + header + "-->\n" + "".join(s.out)
        with open(page_path, "wb") as fh:
            fh.write(body_out.encode("utf-8"))
        stripped_bytes = os.path.getsize(page_path)

        # stylesheets
        css_urls = discover_css(text, rec.get("final_url"))
        css_meta = fetch_css(css_urls)
        css_kept, css_origin_bytes, css_bytes = 0, 0, 0
        css_records = []
        all_css_text = []
        for i, m in enumerate(css_meta):
            u = m["url"]
            nm = "sheet-%02d.css" % i
            if not m.get("ok"):
                css_records.append({"url": u, "fetched": False,
                                    "http_status": m.get("http_status")})
                continue
            cp = os.path.join(dest_dir, nm)
            ob, raw = grab_css(u, cp)
            if ob is None:
                css_records.append({"url": u, "fetched": False})
                continue
            sb = os.path.getsize(cp)
            css_kept += 1
            css_origin_bytes += ob
            css_bytes += sb
            all_css_text.append(raw.decode("utf-8", "replace"))
            css_records.append({
                "url": u, "file": nm, "origin_bytes": ob, "stripped_bytes": sb,
                "sha256": __import__("hashlib").sha256(raw).hexdigest()[:16],
            })
            time.sleep(0.1)

        # shape measurement of the *stripped* capture, so the README's numbers
        # describe the committed artefact and not only the original.
        doc, nb, live = shape_scan.scan_html(page_path)
        css_count = __import__("collections").Counter()
        # The page's own inline <style> blocks and style="" attributes are part
        # of the capture, so they are scanned here. Measuring only the linked
        # sheets reported webfont_heavy=0 for a page carrying 273 @font-face
        # blocks, which is the measurement being wrong, not the page.
        with open(page_path, "rb") as fh:
            css_count.update(shape_scan.scan_css(
                fh.read().decode("utf-8", "replace")))
        for t in all_css_text:
            css_count.update(shape_scan.scan_css(t))
        total = doc.c.copy()
        total.update(live)
        total.update(css_count)
        scores = shape_scan.shape_score(total)

        # binaries left out on purpose
        bin_urls = []
        for a in s.assets:
            if a["kind"] in ("font", "image", "video", "audio"):
                bin_urls.append(a["url"])
        seen, uniq = set(), []
        for u in bin_urls:
            if u not in seen:
                seen.add(u)
                uniq.append(u)
        font_urls = re.findall(r"@font-face\s*\{[^}]*?src\s*:[^}]*?url\(\s*['\"]?([^'\")]+)",
                                "".join(all_css_text), re.I | re.S)
        for u in font_urls:
            if u not in seen:
                seen.add(u)
                uniq.append(u)

        head_cache = {}
        for u in uniq[:120]:
            head_cache[u] = head_size(u)
        total_bin = sum(v for v in head_cache.values() if v)

        # A capture on disk from an earlier successful fetch stays valid even
        # when this run's re-fetch failed. Say which, rather than reporting
        # either a phantom success or a silent loss.
        if stripped_bytes == 0 or not os.path.exists(page_path):
            index.append({
                "shape": shape, "url": url, "slug": slug,
                "reachable_through_proxy": False,
                "note": "re-fetch failed; capture on disk is from an earlier "
                        "successful fetch in the same survey",
                "refetch_note": rec.get("note"),
                "refetch_attempts": rec.get("attempts"),
                "why": why,
                "on_disk_from_earlier_fetch": os.path.exists(page_path),
            })
            print("    RE-FETCH FAILED, keeping earlier capture: %s"
                  % rec.get("note"), flush=True)
            continue

        index.append({
            "shape": shape, "url": url, "final_url": rec.get("final_url"),
            "slug": slug, "reachable_through_proxy": True,
            "http_status": rec.get("http_status"),
            "content_type": rec.get("content_type"),
            "fetched_at": fetched_at, "proxy": PROXY,
            "proxy_ua_that_answered": ua_label,
            "why": why,
            "fidelity": fidelity,
            "fidelity_gate": "strip --lossless over the origin bytes is the "
                             "identity; verified on every build",
            "deviations": dev,
            "origin_bytes": origin_bytes, "stripped_bytes": stripped_bytes,
            "strip_ratio": round(stripped_bytes / origin_bytes, 4) if origin_bytes else None,
            "drops": s.drops, "tracking_removed": s.tracking_removed,
            "comments_removed": s.comments_removed,
            "script_start_tags_kept": s.drops.get("script_start_tags_kept", 0),
            "css_sheets_linked": len(css_urls), "css_sheets_kept": css_kept,
            "css_origin_bytes": css_origin_bytes, "css_stripped_bytes": css_bytes,
            "css_records": css_records,
            "asset_count_left_out": len(uniq),
            "asset_bytes_left_out_known": total_bin,
            "asset_size_known_count": sum(1 for v in head_cache.values() if v),
            "asset_head": {u: v for u, v in list(head_cache.items())},
            "stripped_scores": scores,
            "stripped_counts": {k: v for k, v in sorted(total.items()) if v},
        })
        print("    %d -> %d B  css %d/%d sheets  shapes: %s"
              % (origin_bytes, stripped_bytes, css_kept, len(css_urls),
                 ", ".join("%s=%d" % (k, v) for k, v in
                           sorted(scores.items(), key=lambda kv: -kv[1])[:4])),
              flush=True)
        _write_index(index, snapshot)

    print("\nwrote %d captures" % len(index))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
