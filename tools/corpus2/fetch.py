#!/usr/bin/env python3
"""Capture fetcher for corpus2.

Fetches a URL through the system proxy at 127.0.0.1:17890 using curl.exe,
records full provenance, and writes the raw body to a scratch directory
*outside* the working tree (the tree is shared with seven other agents and a
couple of hundred megabytes of raw HTML would slow every one of their
incremental builds).

Provenance recorded per capture:
  url, final_url, http_status, content_type, origin_bytes, sha256, ok, note

The scratch directory defaults to the approved temp dir. Nothing here ever
writes into the repository.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import paths  # noqa: E402

PROXY = paths.PROXY
SCRATCH = paths.SCRATCH
UA = paths.UA


def slug(url: str) -> str:
    s = re_sub = "".join(
        ch if (ch.isalnum() or ch in "._-") else "_" for ch in url
    )
    return s[:150]


def fetch(url: str, name: str, max_time: int = 40, accept: str | None = None) -> dict:
    os.makedirs(SCRATCH, exist_ok=True)
    body = os.path.join(SCRATCH, name + ".body")
    hdr = os.path.join(SCRATCH, name + ".hdr")
    cmd = [
        "curl.exe", "-sS", "--compressed", "-L",
        "--max-time", str(max_time),
        "--max-filesize", str(64 * 1024 * 1024),
        "--proxy", PROXY,
        "-A", UA,
        "-D", hdr,
        "-o", body,
        "-w", "%{http_code}\t%{url_effective}\t%{content_type}\t%{size_download}\t%{time_total}",
    ]
    if accept:
        cmd += ["-H", "Accept: " + accept]
    cmd.append(url)

    rec = {
        "url": url,
        "name": name,
        "requested_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "proxy": PROXY,
    }
    # Two user agents. Some hosts (www.w3.org among them, behind bot
    # management) return 403 to a browser-shaped UA and 200 to curl's
    # default. Which one answered is recorded, because it is provenance.
    attempts = [("browser-ua", UA), ("curl-default", None)]
    p = None
    out = ""
    trail = []
    for label, ua in attempts:
        cmd2 = list(cmd)
        if ua is None:
            cmd2 = [x for x in cmd2 if x != "-A" and x != UA]
        try:
            p = subprocess.run(cmd2, capture_output=True, timeout=max_time + 20)
        except subprocess.TimeoutExpired:
            trail.append({"ua": label, "result": "subprocess timeout"})
            rec.update(ok=False, note="subprocess timeout",
                       origin_bytes=0, http_status=0, attempts=trail)
            return rec
        out = p.stdout.decode("utf-8", "replace").strip()
        parts = out.split("\t")
        trail.append({
            "ua": label,
            "http_status": parts[0] if parts else None,
            "curl_rc": p.returncode,
        })
        if p.returncode == 0 and len(parts) >= 5 and parts[0].startswith("2"):
            rec["ua_label"] = label
            break
        if p.returncode != 0:
            # A TLS or connection failure is not a user-agent problem.
            rec["ua_label"] = label
            break
    rec["attempts"] = trail
    err = p.stderr.decode("utf-8", "replace").strip()
    parts = out.split("\t")
    if p.returncode != 0 or len(parts) < 5:
        rec.update(ok=False, note="curl rc=%d %s" % (p.returncode, err or out),
                   http_status=0, origin_bytes=0)
        return rec
    status, final_url, ctype, size, ttotal = parts[0], parts[1], parts[2], parts[3], parts[4]
    try:
        size = int(float(size))
    except ValueError:
        size = 0
    digest = ""
    if os.path.exists(body):
        h = hashlib.sha256()
        with open(body, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                h.update(chunk)
        digest = h.hexdigest()
        size = os.path.getsize(body)
    rec.update(
        ok=(status.startswith("2") and size > 0),
        http_status=int(status) if status.isdigit() else 0,
        final_url=final_url,
        content_type=ctype,
        origin_bytes=size,
        sha256=digest,
        seconds=float(ttotal) if ttotal.replace(".", "").isdigit() else None,
        note="" if status.startswith("2") else "http %s" % status,
    )
    return rec


def main() -> int:
    args = sys.argv[1:]
    out_path = None
    urls = []
    i = 0
    while i < len(args):
        a = args[i]
        if a == "--out":
            out_path = args[i + 1]
            i += 2
            continue
        if a == "--name":
            urls.append(("__name__", args[i + 1], args[i + 2]))
            i += 3
            continue
        urls.append(("__auto__", a, None))
        i += 1

    recs = []
    for mode, a, b in urls:
        if mode == "__name__":
            name, url = a, b
        else:
            url = a
            name = slug(url)
        r = fetch(url, name)
        r["ok"] = r.get("ok", False)
        print("%-6s %8s B  %s  %s" % (
            r.get("http_status"), r.get("origin_bytes"),
            r.get("content_type"), url), flush=True)
        if not r["ok"] and r.get("note"):
            print("        note: %s" % r["note"], flush=True)
        recs.append(r)

    if out_path:
        with open(out_path, "w", encoding="utf-8") as fh:
            json.dump(recs, fh, indent=1, sort_keys=True)
    else:
        print(json.dumps(recs, indent=1, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
