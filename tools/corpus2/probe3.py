#!/usr/bin/env python3
"""One-off targeted probe for the sub-features the matrix reports ABSENT.

Run against live pages to establish whether an absence is a property of the
web or a property of this survey. Prints only pages that hit.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from html.parser import HTMLParser

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402

SCRATCH = paths.SCRATCH
PROXY = paths.PROXY
UA = paths.UA
os.makedirs(SCRATCH, exist_ok=True)

VOID = {"area", "base", "br", "col", "embed", "hr", "img", "input", "link",
        "meta", "param", "source", "track", "wbr"}
LISTED = {"button", "fieldset", "input", "object", "output", "select", "textarea"}

FORM_TOKEN = re.compile(rb"<form\b", re.I)
FORM_ATTR = re.compile(rb"<(?:input|button|select|textarea|object|output|img|fieldset)\b[^>]{0,600}?\sform\s*=", re.I)
TFOOT = re.compile(rb"<tfoot\b", re.I)
COLGROUP = re.compile(rb"<colgroup\b", re.I)
TABLE_RULES = re.compile(rb"<table\b[^>]{0,400}?\srules\s*=", re.I)
COLUMN_WIDTH = re.compile(rb"column-width\s*:", re.I)
FO = re.compile(rb"<foreignObject\b", re.I)
CDATA = re.compile(rb"<!\[CDATA\[", re.I)
XMLSPACE = re.compile(rb"xml:space", re.I)
XLINK = re.compile(rb"xlink:href", re.I)
ANIM_DELAY = re.compile(rb"animation-delay\s*:", re.I)
TRANS_DELAY = re.compile(rb"transition-delay\s*:", re.I)
FONT_VAR = re.compile(rb"font-variation-settings\s*:", re.I)
WILDCARD = re.compile(rb"(?m)^\s*\*[a-z-]+\s*:", re.I)


def fetch(url, name):
    """Two user agents, because www.w3.org returns 403 to a browser-shaped UA
    and 200 to curl's default. Which one answered is returned so a 403 can be
    attributed correctly instead of being read as an unreachable host."""
    body = os.path.join(SCRATCH, "_p_" + name)
    last = ""
    for label, ua in (("browser-ua", UA), ("curl-default", None)):
        cmd = ["curl.exe", "-sS", "--compressed", "-L", "--max-time", "45",
               "--max-filesize", str(48 * 1024 * 1024), "--proxy", PROXY,
               "-o", body, "-w", "%{http_code}"]
        if ua:
            cmd += ["-A", ua]
        cmd.append(url)
        p = subprocess.run(cmd, capture_output=True, timeout=70)
        last = p.stdout.decode().strip() or p.stderr.decode()[:80]
        if p.returncode == 0 and last.startswith("2"):
            return open(body, "rb").read(), "200/" + label
        if p.returncode != 0:
            break
    return None, last


class NestedFormProbe(HTMLParser):
    """Counts <form> start tags seen while the form element pointer is set.

    html.parser is not a conforming HTML5 tree builder, so this is a *token*
    count: a second <form> start tag before the first has closed. In a real
    parse the second is ignored and its controls belong to the outer form,
    which is exactly the form-element-pointer case. Reported as a token
    count, never as a claim about the parsed tree.
    """

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.open_forms = 0
        self.nested = 0
        self.controls_between = 0

    def handle_starttag(self, tag, attrs):
        if tag == "form":
            if self.open_forms:
                self.nested += 1
            else:
                self.open_forms = 1
        elif tag in LISTED and self.open_forms:
            pass

    def handle_endtag(self, tag):
        if tag == "form" and self.open_forms:
            self.open_forms = 0


def read_probe_urls(listfile):
    """The URLs in a probe list, with its group headers removed.

    A probe list is shape-grouped, so its first line is a bare label like
    `form`. That label used to be fetched, which reported a fake unreachable
    and made the printed "probed N urls" wrong - and the denominator of a probe
    count is the count.
    """
    if not os.path.exists(listfile):
        listfile = os.path.join(paths.DATA, os.path.basename(listfile))
    with open(listfile, encoding="utf-8") as fh:
        return [l.strip() for l in fh
                if l.strip() and not l.startswith("#")
                and l.strip() != "EOF"
                and not ("://" not in l and " " not in l.strip())]


def main():
    listfile = sys.argv[1]
    # `probe_form_attr2.txt` is the second round at the same question, so a
    # trailing round number must not become part of the probe's name - which it
    # did, and the run died on a KeyError with no hits printed.
    stem = os.path.splitext(os.path.basename(listfile))[0]
    target = re.sub(r"probe_", "", stem)
    target = re.sub(r"[0-9]+$", "", target) or target
    urls = read_probe_urls(listfile)
    print("probing %d urls for %s\n" % (len(urls), target))

    probe = {
        "form_token": FORM_TOKEN, "form_attr": FORM_ATTR,
        "tfoot": TFOOT, "colgroup": COLGROUP, "table_rules": TABLE_RULES,
        "column_width": COLUMN_WIDTH, "foreignObject": FO, "cdata": CDATA,
        "xml_space": XMLSPACE, "xlink_href": XLINK,
        "animation_delay": ANIM_DELAY, "transition_delay": TRANS_DELAY,
        "font_variation_settings": FONT_VAR, "star_hack": WILDCARD,
    }[target]

    hits, fails = [], []
    for u in urls:
        name = re.sub(r"[^A-Za-z0-9._-]", "_", u)[:110]
        data, status = fetch(u, name)
        if data is None:
            fails.append((u, status))
            print("  %-3s %s" % (status, u), flush=True)
            continue
        n = len(probe.findall(data))
        extra = ""
        if target == "form_token":
            p = NestedFormProbe()
            try:
                p.feed(data.decode("utf-8", "replace"))
                p.close()
            except Exception:
                pass
            extra = " nested_form_start_tags=%d" % p.nested
            n = p.nested
        if n:
            hits.append((n, u))
            print("  HIT %-4d %s%s" % (n, u, extra), flush=True)
    print("\n%s: %d hits, %d unreachable" % (target, len(hits), len(fails)))
    for u, s in fails:
        print("  unreachable: %s (%s)" % (u, s))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
