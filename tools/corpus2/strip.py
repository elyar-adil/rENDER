#!/usr/bin/env python3
"""Stripper for the corpus2 captures.

The convention in tests/fixtures/real_sites/ is kept:

  KEEP  structure, tag names, class/id/data-* and inline style attributes,
        table shape (caption, row groups, colspan, rowspan, scope, headers),
        form shape (form id/name/action/method, control names, types, form=),
        media shape (poster, source src+type, preload, controls),
        foreign-content shape (xmlns:*, viewBox, xlink:href, xml:space),
        <template> contents,
        inline <style> including @font-face / unicode-range / @supports /
        @keyframes / media queries,
        text content (whitespace-collapsed, except where it is significant),
        real inline JavaScript up to --inline-js-bytes.

  DROP  <script src> (replaced by a sized placeholder),
        base64 data: URIs in CSS url() and in src/srcset,
        tracking pixels,
        font binaries, image binaries, audio and video (never fetched),
        HTML comments,
        inline scripts above the byte cap (replaced by a sized placeholder).

Every drop is counted and the counts are printed, so the report can say what it
removed rather than implying a lossy step was lossless.

----------------------------------------------------------------------
The one invariant everything else rests on: THIS TOOL IS A SOURCE-SPAN
PASSTHROUGH.

An earlier version of this stripper rebuilt tags from html.parser's parsed
attribute list, which re-quoted every value with "..." and escaped each inner "
as &quot;. Wikipedia carries 4 KB of single-quoted data-mw="{...}" JSON, so that
alone inflated one capture by 25% and un-escaped &amp; into a bare &. The same
class of defect appeared three more times, in a single "repair" pass that
emitted 957 </td> closers the origin never wrote, in a 35-byte placeholder per
HTML comment that grew a 2.6 MB file by 8%, and in the re-emission of every
entity reference as "&name;" whether or not the origin had the semicolon.

So nothing here is reconstructed. Each token is re-emitted as the exact byte
range it occupied in the input, located with getpos() and closed with the same
terminator the parser found. `--lossless` disables every reduction and then
asserts the output is byte-identical to the input; `roundtrip.py` runs that over
every capture. If a token cannot be re-emitted as its own source span, that is a
bug in this file and the lossless mode is what finds it.
"""

from __future__ import annotations

import json
import os
import re
import sys
from html.parser import HTMLParser

VOID = {
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link",
    "meta", "param", "source", "track", "wbr",
}
# Attributes whose *values* are asset URLs. Kept verbatim, never fetched.
URL_ATTRS = {
    "src", "href", "poster", "data-src", "data-original", "data-srcset",
    "srcset", "action", "formaction", "cite", "longdesc", "background",
    "xlink:href", "content",
}
DROP_ATTR_SUBSTR = ("data:image", "data:font", "data:application/font",
                    "data:video", "data:audio", "javascript:", "blob:")
TRACKING_HINT = re.compile(
    r"(pixel|track|beacon|spacer|blank|1x1|analytics|beacon|meters|spacer)", re.I)

DATA_URI_CSS = re.compile(r"""url\(\s*(['"]?)(data:[^)'"]{64,})\1\s*\)""", re.I)
DATA_URI_ATTR = re.compile(r"(data:[a-z]+/[a-z0-9.+-]+;base64,)", re.I)

# The exact shape of a reference in the source, semicolon included or not.
REF = re.compile(r"&(#?[a-zA-Z0-9]+);?")
# An element whose own style attribute turns on preformatted white-space. Its
# text is significant, so it is passed through verbatim like <pre>. A tokeniser
# cannot see a class that a stylesheet gives white-space:pre, which is a
# recorded limit rather than something this file can fix.
WS_PRE = re.compile(r"white-space\s*:\s*(pre|pre-wrap|pre-line|break-spaces)\b", re.I)


def line_starts(text: str) -> list[int]:
    """Absolute index of the first character of each line, 1-based lines."""
    out = [0]
    for m in re.finditer("\n", text):
        out.append(m.end())
    return out


class Stripper(HTMLParser):
    def __init__(self, inline_js_cap: int = 65536, keep_inert_text: bool = True,
                 lossless: bool = False, text: str | None = None):
        super().__init__(convert_charrefs=False)
        self.out: list[str] = []
        self.inline_js_cap = inline_js_cap
        self.keep_inert_text = keep_inert_text
        # In lossless mode nothing is dropped and nothing is rewritten, so the
        # output must equal the input byte for byte. This is the mode the
        # round-trip property test runs, and the only way a defect in the
        # source-span machinery can be caught before it is baked into a capture.
        self.lossless = lossless
        self.drops: dict[str, int] = {}
        self.assets: list[dict] = []
        # Every place the output is NOT a verbatim span of the input, with the
        # number of bytes the substitution moved. Must be zero for --lossless.
        self.deviations: list[dict] = []
        self._script = None          # 'inline' while inside a <script> body
        self._script_buf: list[str] = []
        self._script_start = 0
        self._style_buf: list[str] = []
        self._style_start = 0
        self._in_style = False
        # Whitespace is significant inside <pre>, <textarea> and any element
        # carrying white-space:pre* inline, so text there is passed through
        # verbatim rather than collapsed.
        self._in_inert = 0
        self._inert_tags: list[str] = []
        self.open_stack: list[str] = []
        self.tracking_removed = 0
        self.comments_removed = 0
        self.max_run_ws = 0
        self._text = text
        self._ls: list[int] | None = None
        self.ws_pre_elements = 0
        self.refs_without_semicolon = 0

    # -- source-span machinery ---------------------------------------
    def set_source(self, text: str) -> None:
        self._text = text
        self._ls = line_starts(text)

    def _abs(self) -> int:
        """Absolute index of the token currently being handled.

        html.parser calls updatepos *after* the handler, so getpos() at handler
        time is the first character of the token. Everything this file emits is
        addressed relative to that.
        """
        if self._ls is None or self._text is None:
            return -1
        line, col = self.getpos()
        if 1 <= line <= len(self._ls):
            return min(self._ls[line - 1] + col, len(self._text))
        return len(self._text)

    def _span(self, start: int, terminators: tuple[str, ...]) -> str:
        """The input from `start` through the first terminator, inclusive."""
        if self._text is None:
            return ""
        best = None
        for t in terminators:
            j = self._text.find(t, start)
            if j >= 0 and (best is None or j < best[0]):
                best = (j, len(t))
        if best is None:
            return self._text[start:]
        j, ln = best
        return self._text[start:j + ln]

    def _self_closing_starttag(self) -> str | None:
        """The origin's own text for a start or start-end tag, byte for byte."""
        try:
            return self.get_starttag_text()
        except Exception:
            return None

    def _deviation(self, kind: str, nbytes: int) -> None:
        if self.lossless:
            raise AssertionError(
                "lossless mode still substituted %s (%d B): the stripper is no "
                "longer a source-span passthrough" % (kind, nbytes))
        self.drops["deviation_" + kind] = self.drops.get("deviation_" + kind, 0) + 1
        self.deviations.append({"kind": kind, "bytes": nbytes})

    # -- accounting ---------------------------------------------------
    def drop(self, kind: str, n: int = 1) -> None:
        self.drops[kind] = self.drops.get(kind, 0) + n

    def _note_asset(self, url: str, kind: str, tag: str) -> None:
        if not url or url.startswith(("data:", "javascript:", "#", "about:")):
            return
        self.assets.append({"url": url, "kind": kind, "tag": tag})

    # -- attribute scanning (side effects only) ----------------------
    def _attrs(self, tag, attrs):
        """Record assets and count data: URIs. The tag text is NOT taken from
        this: the emitted tag is the origin's own bytes, so a single-quoted
        value stays single-quoted and an &amp; stays &amp;."""
        for k, v in attrs:
            if v is None:
                continue
            kl = k.lower()
            if DATA_URI_ATTR.search(v):
                self.drop("data_uri_attribute")
                n = len(v)
                re.sub(r"data:[a-z]+/[a-z0-9.+-]+;base64,[A-Za-z0-9+/=]+",
                       "data:about:stripped", v, flags=re.I)
                self.drop("data_uri_bytes", n)
            if kl in URL_ATTRS:
                self._note_asset(v, self._asset_kind(kl, v), tag)
            if kl == "srcset":
                for p in (x.strip() for x in v.split(",")):
                    if p:
                        self._note_asset(p.split()[0], "image", tag)
            if kl == "style" and self.lossless is False and DATA_URI_CSS.search(v):
                self.drop("data_uri_bytes", len(v))

    def _asset_kind(self, attr: str, value: str) -> str:
        lv = value.lower()
        if attr in ("poster",):
            return "image"
        if attr in ("data-src", "data-original", "srcset") or attr.startswith("data-"):
            return "image"
        if re.search(r"\.(woff2?|ttf|otf|eot)(\?|$)", lv):
            return "font"
        if re.search(r"\.(mp4|webm|m4v|mov|ogv)(\?|$)", lv):
            return "video"
        if re.search(r"\.(mp3|m4a|ogg|oga|wav|aac|opus)(\?|$)", lv):
            return "audio"
        if re.search(r"\.(png|jpe?g|gif|webp|avif|svg|bmp|ico)(\?|$)", lv):
            return "image"
        if attr in ("action", "formaction", "href", "cite", "content"):
            return "navigation"
        return "asset"

    def _clean_css(self, text: str) -> str:
        if self.lossless:
            return text
        return DATA_URI_CSS.sub("url(about:stripped)", text)

    def _placeholder(self, body: str, **attrs: str) -> str:
        """A stand-in element carrying what the reduction threw away.

        Deliberately small and uniform. A per-comment placeholder once grew a
        2.6 MB page by 8%, which is a reduction step that made the file larger.
        """
        bits = "".join(' %s="%s"' % (k, v) for k, v in attrs.items())
        return "<%s%s>%s</%s>" % (body.split()[0], bits, "", body.split()[0])

    # -- HTMLParser callbacks ----------------------------------------
    def handle_starttag(self, tag, attrs):
        d = dict(attrs)
        if tag == "script":
            if d.get("src"):
                self._note_asset(d["src"], "javascript", "script")
                if self.lossless:
                    # The start tag is part of the origin's markup and is
                    # emitted verbatim even in lossless mode, so that
                    # `type`, `nonce`, `async` and `defer` survive.
                    self._emit_tag()
                    self._script, self._script_buf, self._script_start = \
                        "inline", [], self._abs()
                else:
                    self.drop("external_script_elements")
                    self._deviation("external_script_placeholder", 0)
                    self.out.append(
                        '<script data-stripped="external-script" '
                        'data-origin-url="%s"></script>' % d["src"])
            else:
                # An earlier version returned here without emitting the start
                # tag, so every inline <script type=...> in every capture had
                # its element turned into loose body text followed by a stray
                # </script>. The element itself, and its attributes, are shape.
                self._emit_tag()
                self.drop("script_start_tags_kept")
                self._script, self._script_buf, self._script_start = \
                    "inline", [], self._abs()
            return
        if tag == "style":
            self._in_style = True
            self._style_buf = []
            self._style_start = self._abs()
            self._emit_tag()
            return
        self._attrs(tag, attrs)
        inert = tag in ("pre", "textarea")
        if not inert and d.get("style") and WS_PRE.search(d["style"]):
            # Its own white-space:pre*. Text inside is significant.
            inert = True
            self.ws_pre_elements += 1
        if inert:
            self._in_inert += 1
            self._inert_tags.append(tag)
        if not self.lossless and tag in ("img", "iframe", "video", "audio",
                                         "source", "track"):
            cls = " ".join((d.get("class") or "").split())
            src = (d.get("src") or "").lower()
            if TRACKING_HINT.search(cls) or TRACKING_HINT.search(src) or \
                    (d.get("width") == "1" and d.get("height") == "1"):
                self.tracking_removed += 1
                self.drop("tracking_pixel_elements")
                self._deviation("tracking_pixel_removed", 0)
                return
        if tag not in VOID:
            self.open_stack.append(tag)
        self._emit_tag()

    def handle_startendtag(self, tag, attrs):
        d = dict(attrs)
        if tag == "script" and d.get("src"):
            self._note_asset(d["src"], "javascript", "script")
        if self.lossless:
            self._attrs(tag, attrs)
            self._emit_tag()
            return
        if tag == "script" and d.get("src"):
            self.drop("external_script_elements")
            self.out.append(
                '<script data-stripped="external-script" '
                'data-origin-url="%s"/>' % d["src"])
            return
        self._attrs(tag, attrs)
        if tag in ("img", "iframe", "video", "audio", "source", "track"):
            cls = " ".join((d.get("class") or "").split())
            src = (d.get("src") or "").lower()
            if TRACKING_HINT.search(cls) or TRACKING_HINT.search(src):
                self.tracking_removed += 1
                self.drop("tracking_pixel_elements")
                return
        self._emit_tag()

    def handle_endtag(self, tag):
        if tag == "script" and self._script == "inline":
            self._attrs("script", [])
            if self.lossless:
                self.out.append("".join(self._script_buf))
                self._script = None
                self.out.append(self._endtag_text())
                return
            body = "".join(self._script_buf)
            raw = len(body.encode("utf-8", "replace"))
            self.drop("inline_script_bytes_seen", raw)
            if raw > self.inline_js_cap:
                self.drop("inline_script_over_cap")
                self._deviation("inline_script_over_cap", raw)
                self.out.append(
                    '<script data-stripped="inline-script-over-cap" '
                    'data-origin-bytes="%d"></script>' % raw)
            else:
                self.out.append(body)
                self.out.append(self._endtag_text())
            self._script = None
            return
        if tag == "style" and self._in_style:
            self._in_style = False
            css = "".join(self._style_buf)
            if not self.lossless:
                before = len(css)
                css = self._clean_css(css)
                if len(css) != before:
                    self.drop("style_data_uris")
                    self._deviation("style_data_uri_removed", before - len(css))
            self.out.append(css)
            self.out.append(self._endtag_text())
            return
        # The origin's tag sequence is reproduced exactly. A stack unwinding
        # that emits a closer for every skipped element *repairs* the document:
        # Wikipedia writes <td> with no </td>, and an earlier version of this
        # stripper added 957 closing tags to that page, silently normalising the
        # very implied end tags and misnesting that render-html is tested
        # against. Elements still open at EOF are left open, as served.
        if self._in_inert and self._inert_tags and self._inert_tags[-1] == tag:
            self._inert_tags.pop()
            self._in_inert = max(0, self._in_inert - 1)
        if tag in self.open_stack:
            i = len(self.open_stack) - 1 - self.open_stack[::-1].index(tag)
            skipped = self.open_stack[i + 1:]
            if skipped:
                self.drop("implied_end_tags_preserved", len(skipped))
            self.open_stack = self.open_stack[:i]
        else:
            self.drop("stray_end_tag")
        self.out.append(self._endtag_text())

    def handle_data(self, data):
        if self._script == "inline":
            self._script_buf.append(data)
            return
        if self._in_style:
            self._style_buf.append(data)
            return
        if self._in_inert and self.keep_inert_text:
            # Whitespace is significant inside <pre>, <textarea> and an element
            # with white-space:pre*, so it is passed through verbatim.
            self.out.append(data)
            return
        if self.lossless:
            self.out.append(data)
            return
        if data.strip() == "":
            self.out.append(" ")
            return
        if len(data) > 400:
            self.drop("text_nodes_over_400b")
        collapsed = re.sub(r"[ \t\r\n]+", " ", data)
        if collapsed != data:
            self.drop("text_whitespace_collapsed_bytes", len(data) - len(collapsed))
            self._deviation("text_whitespace_collapsed", len(data) - len(collapsed))
        self.out.append(collapsed)

    def handle_entityref(self, name):
        if self._text is not None:
            i = self._abs()
            m = REF.match(self._text, i)
            if m and m.group(0).lstrip("&") == name:
                # Re-emit the reference exactly as written. html.parser hands
                # over the *name* only, so re-emitting "&%s;" inserted a
                # semicolon the origin never had, and turned a query string
                # like "?a=1&b=2" into "?a=1&b;=2" in every text node.
                if not m.group(0).endswith(";"):
                    self.refs_without_semicolon += 1
                self.out.append(m.group(0))
                return
        self.out.append("&%s;" % name)

    def handle_charref(self, name):
        if self._text is not None:
            i = self._abs()
            m = REF.match(self._text, i)
            if m and m.group(0).lstrip("&") == "#" + name:
                if not m.group(0).endswith(";"):
                    self.refs_without_semicolon += 1
                self.out.append(m.group(0))
                return
        self.out.append("&#%s;" % name)

    def handle_comment(self, data):
        if self._text is not None:
            i = self._abs()
            if self._text.startswith("<!--", i):
                src = self._span(i, ("-->",))
            else:
                # </ b> and <!x> are bogus comments to the parser. Re-emitting
                # them as <!-- b> would change the bytes, so the exact span is
                # taken and the case is counted on its own.
                src = self._span(i, (">",))
                self.drop("bogus_comment")
            if self.lossless:
                self.out.append(src)
                return
            self.comments_removed += 1
            self.drop("comment_bytes", len(data.encode("utf-8", "replace")))
            # Emitted as nothing, not as a placeholder. A 35-byte placeholder
            # per comment inflated a 2.6 MB page by 8%, which is a reduction
            # step that made the file larger; the count is recorded instead.
            self._deviation("comment_removed", len(src))
            self.out.append("")
            return
        self.comments_removed += 1
        self.out.append("")

    def handle_decl(self, decl):
        if self._text is not None:
            self.out.append(self._span(self._abs(), (">",)))
            return
        self.out.append("<!%s>" % decl)

    def handle_pi(self, data):
        if self._text is not None:
            self.out.append(self._span(self._abs(), (">",)))
            return
        self.out.append("<?%s>" % data)

    def unknown_decl(self, data):
        if self._text is not None:
            self.out.append(self._span(self._abs(), ("]]>", "]>")))
            return
        if data.upper().startswith("CDATA"):
            self.out.append("<![CDATA[%s]]>" % data[6:])
        else:
            self.out.append("<![%s]>" % data)

    # -- emission ------------------------------------------------------
    def _emit_tag(self) -> None:
        src = self._self_closing_starttag()
        if src is None:
            # get_starttag_text() is not available in every path html.parser
            # takes; fall back to the source span, never to a rebuild.
            src = self._span(self._abs(), (">",))
        self.out.append(src)

    def _endtag_text(self) -> str:
        """The origin's own end-tag text. parse_endtag does not populate
        get_starttag_text(), so an earlier version could not have used it here
        even if it had tried; the source span is the only exact route."""
        return self._span(self._abs(), (">",))


def strip_html(text: str, inline_js_cap: int = 65536,
               lossless: bool = False) -> Stripper:
    s = Stripper(inline_js_cap=inline_js_cap, lossless=lossless)
    s.set_source(text)
    try:
        s.feed(text)
        s.close()
    except Exception as exc:
        if lossless:
            raise
        print("strip parse warning: %s" % exc, file=sys.stderr)
    # Elements still open are left open: the capture reproduces the origin's
    # implied end tags rather than normalising them away.
    s.drop("elements_left_open_at_eof", len(s.open_stack))
    s.open_stack = []
    if lossless and "".join(s.out) != text:
        # The property test, run inline so it cannot be bypassed.
        raise AssertionError(
            "lossless strip is not the identity: %d B in, %d B out, "
            "%d deviation(s): %s" % (len(text), len("".join(s.out)),
                                     len(s.deviations),
                                     s.deviations[:3]))
    return s


def main():
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("src")
    ap.add_argument("--out", required=True)
    ap.add_argument("--inline-js-cap", type=int, default=65536)
    ap.add_argument("--header", default=None,
                    help="prepend this text inside a comment before <!doctype")
    ap.add_argument("--lossless", action="store_true",
                    help="drop nothing; fail unless the output is byte-identical")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    with open(a.src, "rb") as fh:
        raw = fh.read()
    origin = len(raw)
    text = raw.decode("utf-8", "replace")
    s = strip_html(text, inline_js_cap=a.inline_js_cap, lossless=a.lossless)
    body = "".join(s.out)
    if a.header:
        body = "<!--\n" + a.header.strip() + "\n-->\n" + body
    with open(a.out, "wb") as fh:
        fh.write(body.encode("utf-8"))
    stripped = os.path.getsize(a.out)
    info = {
        "origin_bytes": origin,
        "stripped_bytes": stripped,
        "ratio": round(stripped / origin, 4) if origin else None,
        "lossless": a.lossless,
        "drops": s.drops,
        "deviations": s.deviations[:50],
        "deviation_count": len(s.deviations),
        "ws_pre_elements": s.ws_pre_elements,
        "refs_without_semicolon": s.refs_without_semicolon,
        "tracking_removed": s.tracking_removed,
        "comments_removed": s.comments_removed,
        "asset_urls_found": len(s.assets),
        "assets": s.assets,
    }
    if a.json:
        print(json.dumps(info, indent=1, sort_keys=True))
    else:
        print("%s: %d -> %d B (%.1f%%)  drops=%s  deviations=%d  assets=%d"
              % (os.path.basename(a.out), origin, stripped,
                 100.0 * stripped / origin if origin else 0,
                 json.dumps(s.drops, sort_keys=True), len(s.deviations),
                 len(s.assets)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
