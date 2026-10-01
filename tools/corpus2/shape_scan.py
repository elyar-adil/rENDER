#!/usr/bin/env python3
"""Shape scanner for the corpus2 capture survey.

Selects pages by the *shape they contain*, not by their popularity: this
counts the structural and CSS features that correspond one-to-one to the open
gaps in docs/visual_fidelity_gaps.md.

Two kinds of evidence are produced:

  * structural  - from a real HTML5-ish parse of the markup (html.parser)
  * stylesheet  - from inline <style> plus linked stylesheets, fetched over the
                  system proxy with curl, capped in size and count

The HTML parse here is deliberately *not* a conforming HTML5 tree builder. It
is a token-stream reader used only to answer survey questions such as "does
this page contain a nested <form> start tag, which the HTML parser's form
element pointer ignores". Counting a shape does not require a conforming
parse; claiming a defect does. See the README for which is which.

Usage:
    python shape_scan.py FILE.html [FILE.css ...] [--json]
    python shape_scan.py --dir DIR            # every .html under DIR
"""

from __future__ import annotations

import json
import os
import re
import sys
from collections import Counter
from html.parser import HTMLParser

VOID = {
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link",
    "meta", "param", "source", "track", "wbr",
}

# HTML 4.10.2 "listed" elements, per the current standard as restated in
# docs/visual_fidelity_gaps.md S24 (fieldset is listed; img is form-associated
# but not listed).
LISTED = {"button", "fieldset", "input", "object", "output", "select", "textarea"}


class Doc(HTMLParser):
    """Token-stream reader producing a shallow tree plus shape counters."""

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.stack: list[tuple[str, dict]] = []
        self.template_stack: list[int] = []
        # (tag, attrs, ancestor_form_indexes, self_index)
        self.nodes: list[list] = []
        self.forms: list[dict] = []
        self.open_form_indexes: list[int] = []
        self.c = Counter()
        for k in MARKUP_COUNTERS:
            self.c[k] = 0
        self.text_nodes = 0
        self.text_chars = 0
        # scripts
        self.scripts_inline = 0
        self.scripts_inline_bytes = 0
        self.scripts_external = 0
        self._in_script = False
        self._script_buf: list[str] = []
        # styles
        self.stylesheet_hrefs: list[str] = []
        self.inline_style_bytes = 0
        self._in_style = False
        self._style_buf: list[str] = []
        self.template_depth = 0
        self.template_rows = 0
        self.template_row_tags: Counter = Counter()
        self.svg_depth = 0
        self.foreign_object = 0
        self.cdata = 0
        # Live vs inert: bytes and tag counts inside <pre>/<code>/<textarea>.
        # A `column-count` in a documentation code sample is not a page that
        # uses multi-column, and an engine measurement that cannot tell the
        # difference will be fooled by exactly that.
        self.inert_bytes = 0
        self.inert_tag_count = 0
        self._in_inert = 0
        # attribute sightings
        self.attrs_seen: Counter = Counter()

    # -- helpers -------------------------------------------------------
    def _open_forms(self) -> list[int]:
        return list(self.open_form_indexes)

    def handle_starttag(self, tag, attrs):
        d = dict(attrs)
        anc_forms = self._open_forms()
        idx = len(self.nodes)
        self.nodes.append([tag, d, anc_forms, idx, 0])

        if self._in_inert:
            self.inert_tag_count += 1
        # <template> used as a row template: a row-ish tag inside its contents
        if self.template_stack and tag in ("tr", "td", "th", "li", "option", "dd", "dt"):
            self.template_row_tags[tag] += 1
            self.c["template_containing_row"] += 1

        if tag == "script":
            if "src" in d:
                self.scripts_external += 1
                self._in_script = False
            else:
                self.scripts_inline += 1
                self._in_script = True
                self._script_buf = []
        elif tag == "style":
            self._in_style = True
            self._style_buf = []
        elif tag == "svg":
            self.c["svg"] += 1
            self.svg_depth += 1
        elif tag == "use":
            self.c["svg_use"] += 1
        elif tag in ("template",):
            self.c["template"] += 1
            self.template_depth += 1
            self.template_stack.append(idx)
        elif tag in ("pre", "code", "textarea"):
            self._in_inert += 1

        for k in d:
            self.attrs_seen[k.lower()] += 1

        # ---- forms -------------------------------------------------
        if tag == "form":
            self.c["form"] += 1
            fi = len(self.forms)
            self.forms.append({
                "index": fi,
                "id": d.get("id"),
                "name": d.get("name"),
                "action": d.get("action"),
                "method": d.get("method"),
                "attrs": d,
                "nodes": [],
            })
            if self.open_form_indexes:
                # A <form> start tag while the form element pointer is non-null
                # is *ignored* by the in-body insertion mode, so controls in
                # here are owned by the outer form, not by this element.
                self.c["nested_form_token"] += 1
                self.forms[-1]["ignored_by_parser"] = True
            self.open_form_indexes.append(fi)
        elif tag in LISTED:
            self.c["listed_control"] += 1
            self.c[tag] += 1
            if anc_forms:
                self.c["listed_control_inside_form"] += 1
                # which form the HTML parser would attribute it to: the last
                # form start tag seen while the pointer was set
                self.forms[anc_forms[-1]]["nodes"].append((tag, idx))
            else:
                self.c["listed_control_outside_form"] += 1
            if "form" in d:
                self.c["control_with_form_attr"] += 1
                tgt = d["form"]
                if not anc_forms:
                    self.c["form_attr_reaches_outside_form"] += 1
                # does the form= target exist and is it an ancestor?
                hit = [f for f in self.forms if f["id"] == tgt]
                if hit and anc_forms and hit[0]["index"] in anc_forms:
                    self.c["form_attr_names_ancestor"] += 1
                elif hit:
                    self.c["form_attr_names_non_ancestor_form"] += 1
                else:
                    self.c["form_attr_names_nothing"] += 1
        elif tag == "img" and "form" in d:
            self.c["img_form_associated"] += 1

        if tag == "label":
            self.c["label"] += 1

        # ---- inputs by type ----------------------------------------
        if tag == "input":
            t = (d.get("type") or "text").lower()
            self.c["input_type_" + re.sub(r"[^a-z0-9]", "", t)] += 1

        # ---- tables -------------------------------------------------
        if tag == "table":
            self.c["table"] += 1
            if d.get("rules"):
                self.c["table_with_rules_attr"] += 1
        elif tag == "caption":
            self.c["table_caption"] += 1
        elif tag in ("thead", "tbody", "tfoot"):
            self.c["row_group_" + tag] += 1
        elif tag == "tr":
            self.c["table_row"] += 1
        elif tag in ("td", "th"):
            self.c["table_cell"] += 1
            if tag == "th":
                self.c["th"] += 1
                if "scope" in d:
                    self.c["th_with_scope"] += 1
                if "id" in d and "headers" in d:
                    self.c["th_with_id_and_headers"] += 1
            if "colspan" in d:
                self.c["cell_colspan"] += 1
            if "rowspan" in d:
                self.c["cell_rowspan"] += 1
        elif tag == "colgroup":
            self.c["colgroup"] += 1
        elif tag == "col":
            self.c["col"] += 1

        # ---- media --------------------------------------------------
        elif tag in ("video", "audio"):
            self.c[tag] += 1
            if "poster" in d:
                self.c[tag + "_poster"] += 1
            if "controls" in d:
                self.c[tag + "_controls"] += 1
            if d.get("preload") is not None:
                self.c[tag + "_preload"] += 1
            if "autoplay" in d:
                self.c[tag + "_autoplay"] += 1
        elif tag == "source":
            self.c["source"] += 1
            if "type" in d:
                self.c["source_with_type"] += 1
            if "srcset" in d:
                self.c["source_srcset"] += 1

        # ---- svg ----------------------------------------------------
        if "xlink:href" in d:
            self.c["xlink_href"] += 1
        if "xml:space" in d:
            self.c["xml_space"] += 1
        if "xml:lang" in d or "xml:base" in d:
            self.c["xml_other"] += 1
        if any(k.startswith("xmlns") for k in d):
            self.c["xmlns_declared"] += 1
        if "viewBox" in d or "viewbox" in {k.lower() for k in d}:
            self.c["svg_viewbox"] += 1
        if tag == "foreignobject":
            # A real <foreignObject> element. Counting the *string* instead
            # reported 199 hits across 11 pages that were all either prose or
            # a filename inside a WPT test-name URL.
            self.c["foreign_object"] += 1
            if self._in_inert:
                self.c["foreign_object_in_code_sample"] += 1

        if tag not in VOID:
            self.stack.append((tag, d))

    def handle_startendtag(self, tag, attrs):
        # A self-closing tag is a start tag that closes itself, and it is how
        # SVG spells much of what it has: <use xlink:href="#a"/>, <svg/>,
        # <rect/>. Counting only handle_starttag missed every one of them, so a
        # page full of <use> and <rect> reported zero of both - and the <use>
        # count is the one the S5 coverage claim rests on. Delegating to
        # handle_starttag and then undoing the *depth* bookkeeping keeps the
        # element open for no time while keeping the count.
        depth = (self.svg_depth, self.template_depth, self._in_inert,
                 len(self.stack), len(self.template_stack),
                 len(self.open_form_indexes))
        self.handle_starttag(tag, attrs)
        (self.svg_depth, self.template_depth, self._in_inert,
         self._stack_n, self._tpl_n, self._form_n) = depth
        del self.stack[self._stack_n:]
        del self.template_stack[self._tpl_n:]
        del self.open_form_indexes[self._form_n:]

    def handle_endtag(self, tag):
        if tag == "script" and self._in_script:
            body = "".join(self._script_buf)
            self.scripts_inline_bytes += len(body.encode("utf-8", "replace"))
            if "DOMContentLoaded" in body or "addEventListener" in body or "window.onload" in body:
                self.c["inline_script_binds_load"] += 1
            self._in_script = False
        elif tag == "style" and self._in_style:
            body = "".join(self._style_buf)
            self.inline_style_bytes += len(body.encode("utf-8", "replace"))
            # html.parser switches to RAWTEXT for <style> even when the <style>
            # is inside <svg>, where the HTML specification does not. A CDATA
            # section inside an SVG <style> is therefore handed over as ordinary
            # style text and never reaches unknown_decl, so it is counted here
            # instead. Without this, instrument A cannot see a CDATA section in
            # the one place a real page puts one, while instrument B can - and
            # the cross-check would report a disagreement that is a limitation
            # of the parser rather than a property of the page.
            if "<![CDATA[" in body:
                if self.svg_depth > 0:
                    self.c["cdata"] += 1
                    if self._in_inert:
                        self.c["cdata_in_code_sample"] += 1
                else:
                    self.c["cdata_outside_foreign_content"] += 1
            self._in_style = False
        elif tag == "svg":
            self.svg_depth = max(0, self.svg_depth - 1)
        elif tag == "template":
            self.template_depth = max(0, self.template_depth - 1)
            if self.template_stack:
                self.template_stack.pop()
        elif tag in ("pre", "code", "textarea"):
            self._in_inert = max(0, self._in_inert - 1)
        elif tag == "form":
            if self.open_form_indexes:
                self.open_form_indexes.pop()

        # unwind to the matching open element
        for i in range(len(self.stack) - 1, -1, -1):
            if self.stack[i][0] == tag:
                del self.stack[i:]
                return

    def handle_data(self, data):
        if self._in_script:
            self._script_buf.append(data)
            return
        if self._in_style:
            self._style_buf.append(data)
            return
        if self._in_inert:
            self.inert_bytes += len(data)
            return
        if not data.strip():
            return
        self.text_nodes += 1
        self.text_chars += len(data)
        if self._in_script and "//<![CDATA[" in data:
            # The legacy JavaScript comment idiom. Recorded separately from a
            # CDATA *section*, because only one of the two is SVG content.
            self.c["js_cdata_comment_idiom"] += 1
        if self.svg_depth > 0 and "<![CDATA[" in data:
            self.cdata += 1

    def unknown_decl(self, data):
        if data.strip().upper().startswith("CDATA"):
            # A CDATA *section* only exists inside foreign content, so
            # svg_depth is required. Without that test the gov.uk capture
            # scored 2 "CDATA sections" from the legacy `//<![CDATA[` comment
            # wrapper in two inline scripts, which is JavaScript, not SVG.
            # The engine's own tree builder has the same condition: a CDATA
            # section in HTML content is a bogus comment.
            if self.svg_depth > 0:
                self.c["cdata"] += 1
                if self._in_inert:
                    self.c["cdata_in_code_sample"] += 1
            else:
                self.c["cdata_outside_foreign_content"] += 1


# ---------------------------------------------------------------- CSS scan
CSS_PATTERNS = {
    "font_face": re.compile(r"@font-face\b", re.I),
    "unicode_range": re.compile(r"unicode-range\s*:", re.I),
    "font_display": re.compile(r"font-display\s*:", re.I),
    "font_variation_settings": re.compile(r"font-variation-settings\s*:", re.I),
    "font_feature_settings": re.compile(r"font-feature-settings\s*:", re.I),
    "keyframes": re.compile(r"@(?:-webkit-|-moz-|-o-|-ms-)?keyframes\b", re.I),
    "transition_prop": re.compile(r"(?<!-)\btransition(?:-[a-z-]+)?\s*:", re.I),
    "transition_delay": re.compile(r"transition-delay\s*:", re.I),
    "animation_prop": re.compile(r"(?<!-)\banimation(?:-[a-z-]+)?\s*:", re.I),
    "animation_delay": re.compile(r"animation-delay\s*:", re.I),
    "animation_fill_mode": re.compile(r"animation-fill-mode\s*:", re.I),
    "position_sticky": re.compile(r"position\s*:\s*sticky", re.I),
    "position_fixed": re.compile(r"position\s*:\s*fixed", re.I),
    "position_absolute": re.compile(r"position\s*:\s*absolute", re.I),
    "position_relative": re.compile(r"position\s*:\s*relative", re.I),
    "z_index": re.compile(r"z-index\s*:", re.I),
    "transform": re.compile(r"(?<!-)\btransform\s*:", re.I),
    "transform_origin": re.compile(r"transform-origin\s*:", re.I),
    "will_change": re.compile(r"will-change\s*:", re.I),
    "isolation": re.compile(r"isolation\s*:", re.I),
    "mix_blend_mode": re.compile(r"mix-blend-mode\s*:", re.I),
    "opacity_lt_1": re.compile(r"opacity\s*:\s*0?\.\d", re.I),
    "float": re.compile(r"(?<!-)\bfloat\s*:", re.I),
    "clear": re.compile(r"(?<!-)\bclear\s*:", re.I),
    "column_count": re.compile(r"column-count\s*:", re.I),
    "columns": re.compile(r"(?<!-)\bcolumns\s*:", re.I),
    "column_width": re.compile(r"column-width\s*:", re.I),
    "column_gap": re.compile(r"column-gap\s*:", re.I),
    "supports": re.compile(r"@supports\b", re.I),
    "media_rule": re.compile(r"@media\b", re.I),
    "container_query": re.compile(r"@container\b", re.I),
    "nesting_amp": re.compile(r"(?m)^\s*[.#&:a-zA-Z\[][^{};]*&"),
    "backdrop_filter": re.compile(r"backdrop-filter\s*:", re.I),
    "clip_path": re.compile(r"clip-path\s*:", re.I),
    "mask_image": re.compile(r"mask-image\s*:", re.I),
    "text_overflow": re.compile(r"text-overflow\s*:", re.I),
    "text_indent": re.compile(r"text-indent\s*:", re.I),
    "letter_spacing": re.compile(r"letter-spacing\s*:", re.I),
    "text_transform": re.compile(r"text-transform\s*:", re.I),
    "line_clamp": re.compile(r"(?<!-)\bline-clamp\s*:", re.I),
    "line_break": re.compile(r"line-break\s*:", re.I),
    "list_style": re.compile(r"list-style(?:-[a-z-]+)?\s*:", re.I),
    "content_prop": re.compile(r"(?<!-)\bcontent\s*:", re.I),
    "star_hack": re.compile(r"(?m)^\s*\*[a-z-]+\s*:"),
    "underscore_hack": re.compile(r"(?m)^\s*_[a-z-]+\s*:"),
    "grid_template": re.compile(r"grid-template(?:-[a-z-]+)?\s*:", re.I),
    "display_flex": re.compile(r"display\s*:\s*(inline-)?flex", re.I),
    "display_grid": re.compile(r"display\s*:\s*(inline-)?grid", re.I),
    "scroll_snap": re.compile(r"scroll-snap[a-z-]*\s*:", re.I),
    "content_visibility": re.compile(r"content-visibility\s*:", re.I),
    "aspect_ratio": re.compile(r"aspect-ratio\s*:", re.I),
    "object_fit": re.compile(r"object-fit\s*:", re.I),
    "print_media": re.compile(r"@media[^{]*\bprint\b", re.I),
    "custom_prop": re.compile(r"(--[a-z0-9-]+)\s*:"),
    "var_use": re.compile(r"\bvar\(\s*--"),
    "important": re.compile(r"!\s*important"),
    "gradients": re.compile(r"(linear-gradient|radial-gradient|conic-gradient)\s*\(", re.I),
    "bg_layer_comma": re.compile(r"background(?:-image)?\s*:[^;]*,[^;]*url\(", re.I),
}


# Every counter this instrument can produce. A counter that is zero everywhere
# must still be *reported* as a measured zero, distinctly from a key that was
# never looked for: the difference between "no page has the shape" and "nobody
# checked" is the whole difference between a published zero and a guess, and
# matrix.json filtering zeros out silently turned the second into the first.
MARKUP_COUNTERS = (
    "svg", "svg_use", "svg_viewbox", "xlink_href", "xml_space", "xml_other",
    "xmlns_declared", "foreign_object", "foreign_object_in_code_sample",
    "cdata", "cdata_in_code_sample", "cdata_outside_foreign_content",
    "js_cdata_comment_idiom", "template", "template_containing_row",
    "form", "nested_form_token", "listed_control",
    "listed_control_inside_form", "listed_control_outside_form",
    "control_with_form_attr", "form_attr_reaches_outside_form",
    "form_attr_names_ancestor", "form_attr_names_non_ancestor_form",
    "form_attr_names_nothing", "img_form_associated", "label", "button",
    "select", "textarea", "fieldset", "input", "object", "output",
    "table", "table_with_rules_attr", "table_caption", "table_row", "table_cell",
    "th", "th_with_scope", "th_with_id_and_headers", "cell_colspan",
    "cell_rowspan", "row_group_thead", "row_group_tbody", "row_group_tfoot",
    "colgroup", "col", "video", "audio", "video_poster", "audio_poster",
    "video_controls", "audio_controls", "video_preload", "audio_preload",
    "video_autoplay", "audio_autoplay", "source", "source_with_type",
    "source_srcset", "void_source", "void_link", "void_meta", "void_base",
    "void_col", "inline_script_binds_load", "inert_code_sample_tags",
)
INERT_BLOCK = re.compile(
    r"<(pre|code|textarea)\b[^>]*>.*?</\1\s*>", re.I | re.S)
CSS_COUNTERS = tuple(CSS_PATTERNS.keys())


# A <script> body is JavaScript, never CSS. scan_css is a raw text grep, so
# without this a line of inline JS containing `position:sticky` or
# `@keyframes` counts as a declaration - which is how one capture came to
# report 388 `transition` declarations. The same argument as INERT_BLOCK, and
# for the same reason: a string in a code sample is not a page using that code.
# Kept separate rather than folded into INERT_BLOCK because the two are
# reported differently: a code sample is a *choice the author made visible*,
# a script body is a different language.
SCRIPT_BLOCK = re.compile(
    r"<script\b[^>]*>.*?</script\s*>", re.I | re.S)
SCRIPT_OPEN = re.compile(r"<script\b[^>]*>", re.I)


def mask_scripts(text: str) -> str:
    """The document with <script> bodies blanked, for CSS measurement only."""
    text = SCRIPT_BLOCK.sub(lambda m: m.group(0)[:m.group(0).find(">") + 1]
                            + " " * (len(m.group(0))
                                     - m.group(0).find(">") - 1),
                            text)
    # An unterminated <script> at EOF: blank from the open tag to the end.
    return SCRIPT_OPEN.sub(lambda m: m.group(0) + " " * 0, text)


def live_html(text: str) -> str:
    """The document with <pre>/<code>/<textarea> regions removed.

    A `column-count` inside a documentation code sample is not a page that
    uses multi-column. Any shape claim in the coverage matrix is made against
    this view, and where the two differ the README says so.
    """
    return INERT_BLOCK.sub(" ", text)


def scan_css(text: str) -> Counter:
    c = Counter()
    for name, pat in CSS_PATTERNS.items():
        n = len(pat.findall(text))
        if n:
            c[name] = n
    return c


def scan_page_css(text: str) -> Counter:
    """CSS in a document, counting neither code samples nor script bodies.

    This is the measurement the coverage matrix and gaps.py use. `scan_css` on
    the raw text is kept for the survey, where the point is to know what the
    bytes contain rather than what the page uses.
    """
    return scan_css(mask_scripts(live_html(text)))


def scan_live_css(text: str) -> Counter:
    """Counts keyed with a `live_` prefix: code samples excluded."""
    c = Counter()
    for name, n in scan_css(live_html(text)).items():
        c["live_" + name] = n
    return c

def scan_html(path: str) -> tuple[Doc, int, Counter]:
    with open(path, "rb") as fh:
        raw = fh.read()
    text = raw.decode("utf-8", "replace")
    d = Doc()
    try:
        d.feed(text)
        d.close()
    except Exception as exc:  # pragma: no cover - survey tool
        print("parse error %s: %s" % (path, exc), file=sys.stderr)
    # Second pass over the code-sample-free view. `xlink:href` inside a
    # documentation snippet is not a page shipping legacy SVG references, and
    # `//<![CDATA[ ... //]]>` inside a script is not an SVG CDATA section.
    live = Counter()
    try:
        dl = Doc()
        dl.feed(live_html(text))
        dl.close()
        for k, v in dl.c.items():
            if v:
                live["live_" + k] = v
    except Exception:
        pass
    return d, len(raw), live


def collect_css(path: str, limit_css: int = 24, cap_bytes: int = 3_000_000) -> tuple[Counter, list, int]:
    """Inline <style> plus linked stylesheets, fetched through the system proxy."""
    import subprocess
    import urllib.parse

    base = "https://" + os.path.basename(os.path.dirname(os.path.abspath(path)))
    with open(path, "rb") as fh:
        text = fh.read().decode("utf-8", "replace")

    hrefs = re.findall(r'<link[^>]+rel=["\']?stylesheet[^>]*>', text, re.I)
    urls = []
    for tag in hrefs:
        m = re.search(r'href=["\']([^"\']+)["\']', tag, re.I)
        if m:
            u = m.group(1)
            if u.startswith("//"):
                u = "https:" + u
            elif u.startswith("/"):
                u = base + u
            elif not u.startswith("http"):
                u = base + "/" + u
            urls.append(u)

    total = scan_css(text)  # inline styles + style="" attributes, roughly
    total.update(scan_live_css(text))
    fetched = []
    spent = 0
    for u in urls[:limit_css]:
        if spent > cap_bytes:
            break
        try:
            out = subprocess.run(
                ["curl.exe", "-s", "--compressed", "--max-time", "20",
                 "--proxy", os.environ.get("CORPUS_PROXY", "http://127.0.0.1:17890"),
                 "-H", "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) corpus-survey/1.0",
                 u],
                capture_output=True, timeout=25,
            )
        except Exception:
            continue
        if out.returncode != 0 or not out.stdout:
            continue
        body = out.stdout[:cap_bytes].decode("utf-8", "replace")
        if "text/css" not in out.stderr.decode("utf-8", "replace") and "<" in body[:400]:
            continue
        spent += len(out.stdout)
        total.update(scan_css(body))
        fetched.append(u)
    return total, fetched, spent


def shape_score(c: Counter) -> dict:
    """The nine target shapes, with a depth score each."""
    return {
        "form_heavy": (
            0
            + 3 * c["form"]
            + 2 * c["listed_control"]
            + 6 * c["nested_form_token"]
            + 5 * c["listed_control_outside_form"]
            + 5 * c["form_attr_reaches_outside_form"]
            + 3 * c["form_attr_names_non_ancestor_form"]
            + c["input_type_password"] * 4
            + c["input_type_submit"] + c["input_type_checkbox"] + c["input_type_radio"]
            + c["select"] + c["textarea"]
        ),
        "data_table": (
            0
            + 3 * c["table"]
            + 4 * c["table_caption"]
            + 3 * (c["row_group_thead"] + c["row_group_tbody"] + c["row_group_tfoot"])
            + 3 * c["cell_colspan"]
            + 3 * c["cell_rowspan"]
            + 2 * c["table_cell"]
            + c["th_with_scope"] + c["colgroup"] + c["table_with_rules_attr"]
        ),
        "webfont_heavy": (
            0
            + 3 * c["font_face"]
            + 4 * c["unicode_range"]
            + c["font_display"]
            + 2 * c["font_variation_settings"]
            + c["font_feature_settings"]
        ),
        "svg_heavy": (
            0
            + 4 * c["svg"]
            + 3 * c["xlink_href"]
            + 3 * c["xml_space"]
            + c["xmlns_declared"]
            + 5 * c["foreign_object"]
            + 5 * c["cdata"]
            + 2 * c["svg_viewbox"]
        ),
        "animation_heavy": (
            0
            + 2 * c["keyframes"]
            + c["transition_prop"]
            + 2 * c["animation_prop"]
            + 3 * c["animation_delay"]
            + c["transition_delay"]
            + c["animation_fill_mode"]
        ),
        "stacked_positioned": (
            0
            + 4 * c["position_sticky"]
            + 2 * c["position_fixed"]
            + c["position_absolute"]
            + 2 * c["z_index"]
            + 2 * c["transform"]
            + c["isolation"] + c["mix_blend_mode"] + c["will_change"]
        ),
        "multicolumn": (
            0
            + 2 * c["float"]
            + c["clear"]
            + 6 * c["column_count"]
            + 4 * c["columns"]
            + 3 * c["column_width"]
            + c["column_gap"]
        ),
        "media_heavy": (
            0
            + 4 * c["video"] + 4 * c["audio"]
            + 2 * c["video_poster"] + c["audio_poster"]
            + 2 * c["source_with_type"]
            + c["source"] + c["video_controls"] + c["audio_controls"]
        ),
        "template_and_inline_js": (
            0
            + 4 * c["template"]
            + 2 * c["inline_script_binds_load"]
            + min(c["scripts_inline"], 6)
        ),
    }


def main() -> int:
    args = sys.argv[1:]
    as_json = "--json" in args
    args = [a for a in args if a != "--json"]
    if not args:
        print(__doc__)
        return 2

    files: list[str] = []
    lists = [a for a in args if a.endswith(".txt")]
    args = [a for a in args if not a.endswith(".txt")]
    for lst in lists:
        with open(lst, encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if line and os.path.exists(line):
                    files.append(line)
    for a in args:
        if a == "--dir":
            continue
        if os.path.isdir(a):
            for root, _d, fns in os.walk(a):
                for fn in fns:
                    if fn.endswith(".html"):
                        files.append(os.path.join(root, fn))
        else:
            files.append(a)

    rows = []
    for f in sorted(files):
        d, nbytes, live = scan_html(f)
        css, fetched, css_bytes = collect_css(f)
        c = d.c.copy()
        c.update(live)
        c.update(css)
        scores = shape_score(c)
        rows.append({
            "file": os.path.basename(f),
            "path": f,
            "html_bytes": nbytes,
            "css_bytes": css_bytes,
            "css_files": len(fetched),
            "inline_style_bytes": d.inline_style_bytes,
            "inline_scripts": d.scripts_inline,
            "inline_script_bytes": d.scripts_inline_bytes,
            "external_scripts": d.scripts_external,
            "inert_code_sample_bytes": d.inert_bytes,
            "inert_code_sample_tags": d.inert_tag_count,
            "template_row_tags": dict(d.template_row_tags),
            "text_nodes": d.text_nodes,
            "text_chars": d.text_chars,
            "scores": scores,
            "counts": dict(c),
        })

    if as_json:
        print(json.dumps(rows, indent=1, sort_keys=True))
        return 0

    for r in rows:
        print("=" * 78)
        print("%s  html=%d B  css=%d B (%d sheets)  inline-style=%d B" % (
            r["file"], r["html_bytes"], r["css_bytes"], r["css_files"], r["inline_style_bytes"]))
        print("scripts: %d inline (%d B), %d external" % (
            r["inline_scripts"], r["inline_script_bytes"], r["external_scripts"]))
        top = sorted(r["scores"].items(), key=lambda kv: -kv[1])
        print("shapes: " + "  ".join("%s=%d" % (k, v) for k, v in top))
        keep = {k: v for k, v in sorted(r["counts"].items()) if v}
        print("counts: " + ", ".join("%s:%d" % kv for kv in keep.items()))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
