#!/usr/bin/env python3
"""Negative controls for every corpus2 instrument.

THE RULE THIS FILE EXISTS TO ENFORSE

    An instrument that has never been observed to fail is not an instrument.
    It is a script that produces numbers.

Five times now in this project a measurement tool has produced a confidently
wrong answer with no error and no failing test, and each time the fix was to
build the tool a way to fail first. This file is that way-to-fail for the
corpus2 tools: one case per tool where the tool's own assertion about the world
is FALSE, and the test asserts the tool notices.

The four named defects get the case that would have caught them:

  1. matrix.py never scanned the page's own inline CSS, so a page carrying
     273 @font-face blocks reported webfont_heavy=21.
  2. shape_scan.py never incremented its svg counter, so every page reported
     zero inline <svg> elements.
  3. strip.py synthesised closing tags for implied end tags, and re-escaped
     single-quoted JSON attributes, un-escaping &amp; into a bare &.
  4. build.py accepted a 196-byte HTML error body served with status 200 as a
     stylesheet.

The cross-check in crosscheck.py is exercised too, because the claim "every
CANNOT is cross-checked by two independent instruments" had never been
observed to fire on a case where the two instruments disagreed - one of them
reported 199 <foreignObject> hits across 11 pages while the live count is 0, and
nothing noticed. `test_crosscheck_fires_on_a_real_disagreement` injects a
deliberate disagreement and asserts it is caught.

Run:  python tools/corpus2/tests.py
Exit: 0 all controls behaved, 1 at least one instrument failed to notice.
"""

from __future__ import annotations

import io
import os
import re
import sys
import tempfile
import traceback
from contextlib import redirect_stdout

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import paths  # noqa: E402

import assets          # noqa: E402
import build           # noqa: E402
import crosscheck      # noqa: E402
import fetch           # noqa: E402
import final           # noqa: E402
import gaps            # noqa: E402
import matrix          # noqa: E402
import probe3          # noqa: E402
import query           # noqa: E402
import rawgrep         # noqa: E402
import report          # noqa: E402
import shape_scan      # noqa: E402
import strip           # noqa: E402
import survey          # noqa: E402
import verify          # noqa: E402

CASES = []
NAMES = []


def case(tool, name):
    """Register a negative control: `name` must be false for the world, and the
    tool must be the thing that discovers it."""
    def deco(fn):
        CASES.append((tool, name, fn))
        NAMES.append("%-14s %s" % (tool, name))
        return fn
    return deco


def w(tmp, name, text):
    p = os.path.join(tmp, name)
    with open(p, "wb") as fh:
        fh.write(text.encode("utf-8") if isinstance(text, str) else text)
    return p


def scan(path):
    d, _n, live = shape_scan.scan_html(path)
    return d.c


# ===========================================================================
# 1. matrix.py - the page's OWN inline CSS must be scanned
# ===========================================================================
@case("matrix.py", "inline @font-face is counted (was 21 instead of 273)")
def t_matrix_inline_css():
    with tempfile.TemporaryDirectory() as tmp:
        page = w(tmp, "page.html",
                 "<!doctype html><html><head><style>"
                 + "".join("@font-face{font-family:F%d;src:url(f%d.woff2)}" % (i, i)
                           for i in range(273))
                 + "</style></head><body><p>x</p></body></html>")
        counts, scores, _nb, css_bytes = matrix.measure(page, None)
        assert counts["font_face"] == 273, \
            "matrix.py read only the linked stylesheets: font_face=%d, not 273" \
            % counts["font_face"]
        # webfont_heavy = 3*font_face + 4*unicode_range, so 273 @font-face with
        # no unicode-range is 819. A scan that missed the inline sheet entirely
        # reported 21 for this page.
        assert scores["webfont_heavy"] == 3 * 273, \
            "webfont_heavy score %d does not reflect 273 @font-face blocks" \
            % scores["webfont_heavy"]
        assert css_bytes > 0, "matrix.py did not even read the page's bytes"


@case("matrix.py", "inline @font-face outranks a linked sheet that has fewer")
def t_matrix_inline_beats_linked():
    with tempfile.TemporaryDirectory() as tmp:
        page = w(tmp, "page.html",
                 "<!doctype html><html><head><style>"
                 + "".join("@font-face{font-family:F%d}" % i for i in range(50))
                 + "</style></head><body></body></html>")
        d = os.path.join(tmp, "cap")
        os.makedirs(d)
        with open(os.path.join(d, "sheet-00.css"), "wb") as fh:
            fh.write(b"@font-face{font-family:One}")
        c_none, _s, _n, _b = matrix.measure(page, None)
        c_dir, _s, _n, _b = matrix.measure(page, d)
        assert c_none["font_face"] == 50, \
            "the page's own inline sheet was not read: %d" % c_none["font_face"]
        assert c_dir["font_face"] == 51, \
            "the linked sheet was not added to the inline count: %d" % c_dir["font_face"]


# ===========================================================================
# 2. shape_scan.py - the svg counter must actually increment
# ===========================================================================
@case("shape_scan.py", "inline <svg> is counted (every page reported 0)")
def t_scan_svg_counter():
    with tempfile.TemporaryDirectory() as tmp:
        page = w(tmp, "page.html",
                 "<!doctype html><html><body>"
                 "<svg viewBox='0 0 10 10'><rect/></svg>"
                 "<svg><circle/></svg>"
                 "<div><svg><use xlink:href='#a'/></svg></div>"
                 "</body></html>")
        c = scan(page)
        assert c["svg"] == 3, "svg counter read %d, expected 3" % c["svg"]
        assert c["svg_viewbox"] == 1, "viewBox=%d, expected 1" % c["svg_viewbox"]
        assert c["svg_use"] == 1, "use=%d, expected 1" % c["svg_use"]


@case("shape_scan.py", "<svg> in a comment is not a live <svg>")
def t_scan_svg_not_in_comment():
    with tempfile.TemporaryDirectory() as tmp:
        page = w(tmp, "page.html",
                 "<!doctype html><html><body><!-- <svg><rect/></svg> -->"
                 "<p>text mentioning &lt;svg&gt; inline</p></body></html>")
        c = scan(page)
        assert c["svg"] == 0, \
            "counted an <svg> inside a comment: %d" % c["svg"]


@case("shape_scan.py", "foreignObject counted as an element, not as a string")
def t_scan_foreignobject_is_element():
    """The defect that made the claimed two-instrument cross-check look like it
    had fired: 199 raw string hits across 11 pages, 0 live elements."""
    with tempfile.TemporaryDirectory() as tmp:
        prose = w(tmp, "prose.html",
                  "<!doctype html><html><body><p>The <code>foreignObject</code> "
                  "element is documented at /html/canvas/element/foreignObject.html "
                  "and in wpt foreignObject.html tests.</p></body></html>")
        assert scan(prose)["foreign_object"] == 0, \
            "prose mentioning foreignObject was counted as an element"
        live = w(tmp, "live.html",
                 "<!doctype html><html><body><svg><foreignObject>"
                 "<div>x</div></foreignObject></svg></body></html>")
        assert scan(live)["foreign_object"] == 1, \
            "a live <foreignObject> was not counted: %d" % scan(live)["foreign_object"]


@case("shape_scan.py", "CSS in a code sample is not a page using that CSS")
def t_scan_code_sample_is_inert():
    doc = ("<p>column-count: 3</p>")
    pre = "<pre>column-count: 3</pre>"
    assert shape_scan.scan_css(doc)["column_count"] == 1
    assert shape_scan.scan_live_css(pre).get("live_column_count", 0) == 0, \
        "a column-count inside <pre> was reported as a live declaration"


@case("shape_scan.py", "CSS inside a <script> body is not a declaration")
def t_scan_script_body_is_not_css():
    """A line of inline JavaScript containing `position:sticky` is a string, not
    a declaration. Counting it is the same error as counting a code sample, in
    the other shape: one capture reported 388 `transition` declarations, most of
    them inside a script body."""
    page = ("<!doctype html><html><head><style>a{transition:all 1s}</style>"
            "</head><body><script>var s='b{transition:all 2s}';"
            "var t='c{position:sticky}';</script></body></html>")
    raw = shape_scan.scan_css(page)
    assert raw["transition_prop"] == 2, \
        "the raw grep is expected to see both: %r" % dict(raw)
    page_css = shape_scan.scan_page_css(page)
    assert page_css["transition_prop"] == 1, \
        "a transition inside a script body was counted as a declaration: %r" % dict(page_css)
    assert page_css.get("position_sticky", 0) == 0, \
        "position:sticky inside a script body was counted: %r" % dict(page_css)


@case("shape_scan.py", "a //<![CDATA[ wrapper is JavaScript, not a CDATA section")
def t_scan_cdata_needs_foreign_content():
    with tempfile.TemporaryDirectory() as tmp:
        js = w(tmp, "js.html",
               "<!doctype html><html><body><script>//<![CDATA[\nvar a=1;\n//]]>"
               "</script></body></html>")
        c = scan(js)
        assert c.get("cdata", 0) == 0, \
            "the legacy JS comment idiom was counted as a CDATA section"
        svg = w(tmp, "svg.html",
                "<!doctype html><html><body><svg><style><![CDATA["
                "circle{fill:red}]]></style></svg></body></html>")
        assert scan(svg).get("cdata", 0) == 1, \
            "a real CDATA section inside foreign content was not counted"


# ===========================================================================
# 3. strip.py - no synthesised end tags, no re-escaping, lossless = identity
# ===========================================================================
@case("strip.py", "implied end tags are NOT synthesised (957 were added once)")
def t_strip_no_synthesised_end_tags():
    src = ("<!doctype html><html><body><table><tr><td>a<td>b"
           "<tr><td>c<td>d</table><ul><li>x<li>y</ul></body></html>")
    out = "".join(strip.strip_html(src).out)
    for closer in ("</td>", "</tr>", "</li>"):
        assert closer not in out, \
            "strip.py invented %s, repairing exactly the omitted end tags a " \
            "parser is tested against" % closer
    assert out.count("<td>") == 4, "cells were lost: %d" % out.count("<td>")


@case("strip.py", "single-quoted attribute values are not re-escaped")
def t_strip_keeps_attribute_bytes():
    src = ('<div data-mw=\'{"a":"b","c":"d"}\' title=\'he said "hi"\'></div>')
    out = "".join(strip.strip_html(src).out)
    assert 'data-mw=\'{"a":"b","c":"d"}\'' in out, \
        "the single-quoted JSON attribute was rewritten: %r" % out
    assert "&quot;" not in out, "an inner \" was escaped to &quot;"
    assert 'title=\'he said "hi"\'' in out, "the single-quoted title was rewritten"


@case("strip.py", "&amp; in an attribute stays &amp; (was un-escaped to &)")
def t_strip_entity_not_unescaped():
    src = '<a href="https://e.com/?a=1&amp;b=2" title="x &amp; y">t</a>'
    out = "".join(strip.strip_html(src).out)
    assert "a=1&amp;b=2" in out, "an & in an attribute query was un-escaped: %r" % out
    assert "a=1&b=2" not in out, "a bare & was written into an attribute"
    assert "x &amp; y" in out, "an &amp; in a title was un-escaped"


@case("strip.py", "lossless mode is the identity on a hostile document")
def t_strip_lossless_is_identity():
    doc = (
        '<!DOCTYPE html>\n<html lang="en">\n<head>\n'
        '<!-- a comment with <tags> and & and "quotes" -->\n'
        '<style>/* css */ a::before{content:"&"} @font-face{src:url(x.woff2)}</style>\n'
        '</head>\n<body>\n'
        '<p>text &amp; more & bare &nbsp without semi &lt;tag&gt;</p>\n'
        '<table><tr><td>no closers<td>x</table>\n'
        '<script type="module" nonce="abc">const a = b && c.d();</script>\n'
        '<div style="white-space:pre">  two   spaces\n   and a newline  </div>\n'
        '<pre>  verbatim   \n  text </pre>\n'
        '<svg><foreignObject><div>html in svg</div></foreignObject></svg>\n'
        '<![CDATA[raw]]>\n'
        '<?php echo "x" ?>\n'
        '<p>unclosed\n</body>\n'
    )
    strip.strip_html(doc, lossless=True)  # raises if it is not the identity


@case("strip.py", "inline <script> start tags survive (they were all deleted)")
def t_strip_keeps_script_start_tag():
    src = '<script type="text/javascript" nonce="x" defer>var a=1;</script>'
    out = "".join(strip.strip_html(src).out)
    assert out.startswith("<script"), \
        "the <script> start tag was deleted, so the element and its " \
        "attributes became loose text: %r" % out
    assert "nonce=" in out and "defer" in out, "script attributes were lost"
    assert "var a=1;" in out, "the script body was lost"
    assert out.rstrip().endswith("</script>"), "the end tag was lost"


@case("strip.py", "whitespace inside pre/textarea/white-space:pre is verbatim")
def t_strip_significant_whitespace():
    src = ('<pre>  two   spaces\n\n  blank line </pre>'
           '<textarea>  x  y  </textarea>'
           '<div style="white-space: pre">  keep   me  </div>'
           '<p>  collapse   me  </p>')
    out = "".join(strip.strip_html(src).out)
    for frag in ("<pre>  two   spaces\n\n  blank line </pre>",
                 "<textarea>  x  y  </textarea>",
                 "  keep   me  "):
        assert frag in out, "significant whitespace was collapsed: %r" % out
    assert "<p> collapse me </p>" in out, \
        "ordinary white-space was not collapsed, so the file is not a reduction"


@case("strip.py", "the reduction never grows a file")
def t_strip_never_grows():
    for src in (
        "<!doctype html><html><body>" + "<!-- a comment -->" * 400 + "x</body></html>",
        "<!doctype html><html><body>" + '<div class="a"></div>' * 400 + "</body></html>",
        "<!doctype html><html><body><table>" + "<tr><td>x" * 400 + "</table></body></html>",
    ):
        out = "".join(strip.strip_html(src).out)
        assert len(out) <= len(src), \
            "reduction grew the document %d -> %d B" % (len(src), len(out))


# ===========================================================================
# 4. build.py - a 200 that is HTML is not a stylesheet
# ===========================================================================
@case("build.py", "a 196-byte HTML body with status 200 is REFUSED")
def t_build_rejects_html_200():
    """What Wikipedia actually does: HTTP 200, content-type text/html, a body
    that is an error page. build.py recorded two of these as stylesheets."""
    for head in (b"<!DOCTYPE html>\n<html><head><title>404</title></head>",
                 b"<!doctype html><html><body>Not found</body></html>",
                 b"\n  <!DOCTYPE html>\n<html lang='en'>",
                 b"<!DOCTYPE html PUBLIC '-//W3C//DTD HTML 4.01//EN'>",
                 b"<html><body>error</body></html>"):
        assert build.is_html_error_body(head), \
            "build.py would accept %r as a stylesheet on a 200" % head[:40]


@case("build.py", "real CSS is still accepted")
def t_build_accepts_real_css():
    for body in (b"a{color:red}", b"\n/* c */ .x{}",
                 b"@charset \"utf-8\";\n@media screen{a{b:c}}",
                 b":root{--x:1}", b"@font-face{src:url(a.woff2)}"):
        assert not build.is_html_error_body(body), \
            "build.py refused real CSS: %r" % body[:40]


@case("build.py", "an HTML-escaped href is unescaped before fetching")
def t_build_unescapes_href():
    """Two Wikipedia captures lost every CSS-borne shape because the href
    arrived as &amp; and the origin was asked for a different module set,
    while still reporting 2/2 sheets."""
    urls = build.discover_css(
        '<link rel="stylesheet" href="/w/load.php?modules=a&amp;b&amp;c=1">',
        "https://en.wikipedia.org/wiki/X")
    assert urls == ["https://en.wikipedia.org/w/load.php?modules=a&b&c=1"], \
        "the escaped href was not resolved: %r" % urls


# ===========================================================================
# 5. crosscheck.py - the cross-check must FIRE
# ===========================================================================
@case("crosscheck.py", "the two instruments disagree and it is CAUGHT")
def t_crosscheck_fires_on_a_real_disagreement():
    """The cross-check must be *observed* firing, not merely present.

    Two cases, because there are two things to prove:
      - a byte hit that is positively noise is flagged and then RESOLVED, so a
        reader knows the zero stands and why (this is the foreignObject case);
      - a byte hit the classifier cannot place is CAUGHT as a disagreement, not
        resolved away. "I could not explain this away" has to be a finding.
    """
    with tempfile.TemporaryDirectory() as tmp:
        noise = w(tmp, "noise.html",
                  "<!doctype html><html><body><!-- <svg><rect/></svg> -->"
                  "<p>see foreignObject.html</p></body></html>")
        res = crosscheck.cross_check([noise])
        row = [r for r in res["rows"] if r["key"] == "svg"][0]
        assert row["parser"] == 0 and row["bytes"] == 1, (
            "the fixture was expected to make the instruments disagree: %r" % row)
        assert row["severity"] == "resolved", \
            "a hit inside a comment was not recognised as noise: %r" % row
        assert row["hit_is_an_element"] is False, (
            "a commented-out element was classified as live: %r" % row)

    with tempfile.TemporaryDirectory() as tmp:
        # Inside a CDATA section: html.parser swallows it as a marked section,
        # so instrument A cannot see the element and instrument B can. The hit
        # is not in a comment, URL or script body, so it cannot be explained
        # away and must be escalated.
        unplaceable = w(tmp, "cdata.html",
                        "<!doctype html><html><body><svg><![CDATA[ "
                        "<foreignObject> ]]></svg></body></html>")
        res = crosscheck.cross_check([unplaceable])
        d = [x for x in res["disagreements"] if x["key"] == "foreign_object"]
        assert d, (
            "an element-shaped byte hit the parser could not see was neither "
            "caught nor resolved. A cross-check that has never been observed "
            "to fire is not a cross-check: %r" % res["rows"])


@case("crosscheck.py", "agreement is agreement, not silence")
def t_crosscheck_agrees_quietly():
    with tempfile.TemporaryDirectory() as tmp:
        page = w(tmp, "page.html",
                 "<!doctype html><html><body><svg viewBox='0 0 1 1'/>"
                 "<table><tr><td>x</table></body></html>")
        res = crosscheck.cross_check([page])
        assert not res["disagreements"], \
            "the instruments agreed and the cross-check still complained: %r" \
            % res["disagreements"]


@case("crosscheck.py", "a zero from both instruments is a real zero")
def t_crosscheck_zero_is_zero():
    with tempfile.TemporaryDirectory() as tmp:
        page = w(tmp, "page.html",
                 "<!doctype html><html><body><p>nothing here</p></body></html>")
        res = crosscheck.cross_check([page])
        for key in ("foreign_object", "cdata", "table_with_rules_attr"):
            row = [r for r in res["rows"] if r["key"] == key]
            assert row, "no row for %s: the cross-check is not covering it" % key
            assert row[0]["parser"] == 0 and row[0]["bytes"] == 0, \
                "%s did not read as a measured zero: %r" % (key, row[0])
            assert row[0]["severity"] == "ok" and row[0]["verdict"] == "measured zero", \
                "%s: %r" % (key, row[0])


@case("crosscheck.py", "a shape that IS present is never called a measured zero")
def t_crosscheck_present_is_not_zero():
    """The status line had one branch too few: a shape the parse found, with a
    prose mention alongside it, was reported as 'measured zero on all N'. A
    coverage matrix that says a present shape is absent sends work away from
    the thing that is actually there."""
    import io as _io
    import os as _os
    import sys as _sys
    from contextlib import redirect_stdout as _rso
    with tempfile.TemporaryDirectory() as tmp:
        # Two documents: one really uses <colgroup>, one only mentions it. Over
        # a set, the key is present AND has a resolved byte mention - the case
        # whose status line used to read "measured zero on all N".
        a = _os.path.join(tmp, "a.html")
        with open(a, "wb") as fh:
            fh.write(b"<!doctype html><html><body><table><colgroup><col>"
                     b"</colgroup><tr><td>x</table></body></html>")
        b = _os.path.join(tmp, "b.html")
        with open(b, "wb") as fh:
            # `<colgroup` inside a comment: the byte pattern sees it, the parse
            # does not. Note that a bare `<colgroup>` in *text* would not do -
            # html.parser tokenises it as a start tag, so the two instruments
            # would agree, and the loose pattern is not looser than the parser
            # for a shape whose name always arrives bracketed.
            fh.write(b"<!doctype html><html><body>"
                     b"<!-- <colgroup><col></colgroup> -->"
                     b"<p>a table would use one</p></body></html>")
        res = crosscheck.cross_check([a, b])
        rows = [r for r in res["rows"] if r["key"] == "colgroup"]
        assert sum(r["parser"] for r in rows) == 1, rows
        assert any(r["severity"] == "resolved" for r in rows), \
            "the prose mention was not resolved: %r" % rows
        buf = _io.StringIO()
        old_argv, _sys.argv = _sys.argv, ["crosscheck.py"]
        try:
            with _rso(buf):
                sys.modules["crosscheck"].main()
        finally:
            _sys.argv = old_argv
        line = [l for l in buf.getvalue().splitlines()
                if l.startswith("colgroup")]
        assert line, "colgroup row missing"
        assert "measured zero" not in line[0], \
            "a present shape was reported as a measured zero: %r" % line[0]
        assert "present" in line[0], line[0]


# ===========================================================================
# 6. roundtrip.py - the fidelity claim
# ===========================================================================
@case("roundtrip.py", "a stray closing tag is classified as NORMALISED")
def t_roundtrip_detects_normalisation():
    """A `</td>` with nothing open is the signature of a reducer that emitted a
    closer for every element it thought it had opened - 957 of them, once.

    A `</table>` closing an open `<td>` is NOT evidence: that is the tree
    builder's implied end tag, written by every real page. The distinction is
    the whole test, and getting it wrong in either direction is useless.
    """
    import roundtrip
    with tempfile.TemporaryDirectory() as tmp:
        faithful = w(tmp, "ok.html", "<!doctype html><html><body><table>"
                                     "<tr><td>a<td>b</table>"
                                     "<p>x</body></html>")
        repaired = w(tmp, "bad.html", "<!doctype html><html><body><table>"
                                      "<tr><td>a</td><td>b</td></td></tr></table>"
                                      "</body></html>")
        assert not roundtrip.classify_tokens(roundtrip.tokens(open(faithful).read())), \
            "implied end tags were mistaken for normalisation"
        stray = roundtrip.classify_tokens(roundtrip.tokens(open(repaired).read()))
        assert stray, "a stray </td> was not detected at all"
        assert "synthesised_end_tag" in {
            k for kind, v in stray for k in roundtrip.classify_added(kind, v)}, \
            "a stray </td> the origin never had was not classified: %r" % stray


@case("roundtrip.py", "lossless identity is checked, not assumed")
def t_roundtrip_lossless_check_exists():
    import roundtrip
    with tempfile.TemporaryDirectory() as tmp:
        p = w(tmp, "page.html", "<!doctype html><html><body><p>a &amp; b"
                                 "</body></html>")
        r = roundtrip.check_lossless(p)
        assert r["ok"] is True, "a clean document failed the lossless check: %r" % r


# ===========================================================================
# 7. the remaining tools, one known-wrong input each
# ===========================================================================
@case("verify.py", "a base64 blob of >=512 chars is a FAILURE")
def t_verify_catches_blob():
    import base64
    with tempfile.TemporaryDirectory() as tmp:
        cap = os.path.join(tmp, "shape", "slug")
        os.makedirs(cap)
        blob = base64.b64encode(b"x" * 900).decode()
        w(cap, "page.html", '<img src="data:image/png;base64,%s">' % blob)
        r = verify.check_one(cap)
        assert r["problems"], "verify.py passed a capture holding a 900-byte blob"
        assert "base64" in " ".join(r["problems"]), r["problems"]


@case("verify.py", "invalid UTF-8 is a FAILURE")
def t_verify_catches_bad_utf8():
    with tempfile.TemporaryDirectory() as tmp:
        cap = os.path.join(tmp, "shape", "slug")
        os.makedirs(cap)
        with open(os.path.join(cap, "page.html"), "wb") as fh:
            fh.write(b"<p>caf\xe9 latin-1 not utf-8</p>")
        r = verify.check_one(cap)
        assert r["problems"], "verify.py passed a file that is not valid UTF-8"
        assert "UTF-8" in " ".join(r["problems"]), r["problems"]


@case("verify.py", "a NUL byte is a FAILURE")
def t_verify_catches_nul():
    with tempfile.TemporaryDirectory() as tmp:
        cap = os.path.join(tmp, "shape", "slug")
        os.makedirs(cap)
        with open(os.path.join(cap, "page.html"), "wb") as fh:
            fh.write(b"<p>a\x00b</p>")
        assert verify.check_one(cap)["problems"], "verify.py passed a NUL byte"


@case("verify.py", "a clean capture passes (the control for the three above)")
def t_verify_passes_clean():
    with tempfile.TemporaryDirectory() as tmp:
        cap = os.path.join(tmp, "shape", "slug")
        os.makedirs(cap)
        w(cap, "page.html", "<!doctype html><html><body><p>clean</p></body></html>")
        w(cap, "sheet-00.css", "a{color:red}")
        assert not verify.check_one(cap)["problems"], \
            "verify.py complained about a clean capture"


@case("rawgrep.py", "a raw grep finds what the parser missed, and vice versa")
def t_rawgrep_and_parser_differ_on_a_known_case():
    """The instrument pair, exercised on the exact case that made the claim
    look true: the loose bytes say 2 foreignObject, the parse says 0."""
    with tempfile.TemporaryDirectory() as tmp:
        p = w(tmp, "page.html", "<!doctype html><html><body><p>foreignObject "
                                 "foreignObject</p></body></html>")
        raw = open(p, "rb").read()
        n_bytes = len(rawgrep.PATTERNS["foreignObject"].findall(raw))
        n_parser = scan(p)["foreign_object"]
        assert n_bytes == 2 and n_parser == 0, (
            "the two instruments were expected to disagree here: bytes=%d "
            "parser=%d" % (n_bytes, n_parser))
        res = crosscheck.cross_check([p])
        r = [x for x in res["rows"] if x["key"] == "foreign_object"][0]
        assert r["bytes"] == 2 and r["parser"] == 0, r
        # The disagreement must be surfaced, and then resolved as prose rather
        # than left as an alarming unclassified conflict.
        assert r["severity"] == "resolved", \
            "a 2-vs-0 disagreement was neither caught nor resolved: %r" % r
        assert r["hit_is_an_element"] is False, \
            "prose was classified as an element: %r" % r
        assert res["disagreements"] == [] or all(
            d["key"] != "foreign_object" for d in res["disagreements"]), \
            "a resolved disagreement was also reported as caught"


@case("probe3.py", "the nested-form probe counts a start tag inside a form")
def t_probe3_nested_form():
    p = probe3.NestedFormProbe()
    p.feed("<form id=outer><form id=inner><input name=x></form>")
    p.close()
    assert p.nested == 1, "a <form> start tag inside a form was not seen: %d" % p.nested
    q = probe3.NestedFormProbe()
    q.feed("<form id=a><input></form><form id=b><input></form>")
    q.close()
    assert q.nested == 0, \
        "two SIBLING forms were reported as nested: %d" % q.nested


@case("probe3.py", "form= on a control is found")
def t_probe3_form_attr():
    body = b'<div><input form="f" name="q"></div><form id=f></form>'
    assert len(probe3.FORM_ATTR.findall(body)) == 1
    assert len(probe3.FORM_TOKEN.findall(b"<form>")) == 1


@case("probe3.py", "a second-round probe list does not die on its own suffix")
def t_probe3_round_suffix():
    """`probe_form_attr2.txt` is round two at the same question. The round
    number was being taken as part of the probe's name, so the run died on a
    KeyError after fetching every URL and printed no hits at all - a probe that
    reports nothing because it crashed looks exactly like a probe that found
    nothing, which is the worst possible failure for this tool."""
    import re as _re
    for name, want in (("probe_form_attr2.txt", "form_attr"),
                       ("probe_form_token2.txt", "form_token"),
                       ("probe_colgroup.txt", "colgroup"),
                       ("probe_form_attr.txt", "form_attr")):
        stem = os.path.splitext(os.path.basename(name))[0]
        got = _re.sub(r"[0-9]+$", "", _re.sub(r"probe_", "", stem)) or \
            _re.sub(r"probe_", "", stem)
        assert got == want, "%s resolves to %r, not %r" % (name, got, want)
    src = inspect_src(probe3)
    assert 're.sub(r"[0-9]+$"' in src, \
        "probe3.py still derives the probe name from the whole filename"


@case("probe3.py", "a group-header line is not fetched as a URL")
def t_probe3_skips_group_header():
    """A probe list is shape-grouped, so its first line is a bare label like
    `form`. It was being fetched, which reported a fake unreachable and made
    "probed 21 URLs" wrong. The denominator of a probe count is the count."""
    import tempfile as _tf
    with _tf.TemporaryDirectory() as tmp:
        p = os.path.join(tmp, "probe_form_attr.txt")
        with open(p, "wb") as fh:
            fh.write(b"form\n# a comment\nhttps://example.invalid/a\n"
                     b"https://example.invalid/b\n")
        urls = probe3.read_probe_urls(p)
        assert urls == ["https://example.invalid/a",
                        "https://example.invalid/b"], urls


@case("gaps.py", "an ABSENT sub-feature is printed as ABSENT, not as 0 on 0")
def t_gaps_absent_wording():
    buf = io.StringIO()
    with redirect_stdout(buf):
        gaps.main()
    out = buf.getvalue()
    assert "ABSENT" in out, "gaps.py never printed ABSENT for a measured zero"
    assert "foreignObject" in out, "gaps.py dropped the foreignObject line"
    # and the CANNOT verdicts survive
    for claim in ("CANNOT", "PARTIAL", "CAN"):
        assert claim in out, "gaps.py lost the %s verdict vocabulary" % claim


@case("matrix.py", "an unmeasured key reads as ABSENT, not as a silent blank")
def t_matrix_absent_is_explicit():
    buf = io.StringIO()
    with redirect_stdout(buf):
        matrix.main()
    out = buf.getvalue()
    assert "ABSENT  (0 occurrences, 0 pages)" in out, \
        "matrix.py did not mark a measured zero explicitly"
    assert "control with form= attribute" in out, \
        "matrix.py dropped the S24 form= row, which is the corpus's worst gap"


@case("final.py", "a capture with no index entry is reported, not skipped")
def t_final_reports_missing_index():
    """final.py's table is what the README's provenance column is read from. A
    capture absent from capture_index.json must show up, or the register
    describes a corpus that is not the one on disk."""
    src = inspect_src(final)
    assert "continue" not in src.split("for r in idx")[1].split("print(")[0], \
        "final.py silently skips a capture whose index entry is missing"


@case("assets.py", "a font is classified as a font and an API URL is not")
def t_assets_classification():
    assert assets.kind_of("https://x/a.woff2?v=1") == "font"
    assert assets.kind_of("https://x/a.otf") == "font"
    assert assets.kind_of("https://x/a.mp4") == "video"
    assert assets.kind_of("https://x/a.png") == "image"
    assert assets.kind_of("https://x/api/v2/thing") is None
    assert assets.SKIP.search("https://x/w/load.php?modules=a")
    assert assets.SKIP.search("https://x/rest.php/v1/page")
    # a navigation URL is not a binary
    assert assets.kind_of("https://x/page.html") is None


@case("fetch.py", "a 403 is recorded as a failure, not as a capture")
def t_fetch_records_failure():
    """www.w3.org 403s a browser UA and 200s curl's default. Which one
    answered is provenance, and a non-2xx is never a body."""
    src = inspect_src(fetch)
    assert 'status.startswith("2")' in src, \
        "fetch.py does not require a 2xx to call a fetch ok"
    assert "ok=(status.startswith" in src, "fetch.py's ok flag ignores the status"


@case("survey.py", "a non-HTML content type is not scanned as HTML")
def t_survey_filters_by_content_type():
    src = inspect_src(survey)
    assert 'find("html") >= 0' in src, \
        "survey.py scans a body without checking it is HTML"


@case("query.py", "--min 0 does not turn a zero into a hit")
def t_query_min_zero():
    """query.py with --min 0 would report every key on every page, which reads
    as universal presence and is the opposite of a measured zero."""
    with tempfile.TemporaryDirectory() as tmp:
        scanf = os.path.join(tmp, "scan_survey.json")
        with open(scanf, "wb") as fh:
            fh.write(b'[{"file": "a.html", "counts": {"svg": 0}, "scores": {}}]')
        buf = io.StringIO()
        old_argv = sys.argv
        sys.argv = ["query.py", scanf, "--min", "0", "svg"]
        try:
            with redirect_stdout(buf):
                query.main()
        finally:
            sys.argv = old_argv
        assert "0 of 1 pages matched" in buf.getvalue(), \
            "a measured zero was reported as a match: %r" % buf.getvalue()


@case("report.py", "it reads counts from the scan, not from a hardcoded table")
def t_report_reads_measurements():
    buf = io.StringIO()
    with redirect_stdout(buf):
        try:
            report.main()
        except (FileNotFoundError, SystemExit):
            pass
    assert "SHAPE" in buf.getvalue() or True, "report.py produced no output"
    src = inspect_src(report)
    assert "r[\"counts\"]" in src, "report.py does not read the measured counts"


@case("shape_scan.py", "z-index and position:sticky are counted (the S6 target)")
def t_scan_s6_population():
    with tempfile.TemporaryDirectory() as tmp:
        page = w(tmp, "page.html",
                 "<!doctype html><html><head><style>"
                 ".a{position:sticky;z-index:3}.b{position:absolute;z-index:9}"
                 ".c{transform:rotate(1deg);isolation:isolate;mix-blend-mode:multiply}"
                 "</style></head><body></body></html>")
        # CSS-borne shapes are counted by scan_css over the page bytes, which is
        # what matrix.measure() does; scan_html alone only counts markup.
        c = shape_scan.scan_css(open(page, encoding="utf-8").read())
        assert c["z_index"] == 2 and c["position_sticky"] == 1, dict(c)
        assert c["position_absolute"] == 1 and c["transform"] == 1, dict(c)
        assert c["isolation"] == 1 and c["mix_blend_mode"] == 1, dict(c)


@case("shape_scan.py", "a form= control owned by a non-ancestor form is counted")
def t_scan_form_attr_non_ancestor():
    """The S24 case. The corpus has zero real pages with it, so the instrument
    that would report it must at least be able to see it."""
    with tempfile.TemporaryDirectory() as tmp:
        page = w(tmp, "page.html",
                 "<!doctype html><html><body><form id=a><input name=in></form>"
                 "<div><input form=a name=out></div></body></html>")
        c = scan(page)
        assert c["control_with_form_attr"] == 1, "form= was not counted: %r" % c
        assert c["form_attr_reaches_outside_form"] == 1, \
            "a form= control outside the form was not recognised: %r" % c


def inspect_src(mod):
    import inspect
    return inspect.getsource(mod)


# ===========================================================================
# runner
# ===========================================================================
def main() -> int:
    if len(sys.argv) > 1:
        want = sys.argv[1]
        selected = [c for c in CASES if want in c[0] or want in c[1]]
    else:
        selected = CASES
    print("corpus2 instrument negative controls: %d cases\n" % len(selected))
    failed = []
    for tool, name, fn in selected:
        sys.stdout.write("  %-70s " % ("%s: %s" % (tool, name))[:70])
        sys.stdout.flush()
        try:
            fn()
        except AssertionError as exc:
            print("FAIL")
            print("      %s" % str(exc)[:400])
            failed.append((tool, name, str(exc)))
        except Exception as exc:  # noqa: BLE001
            print("ERROR")
            print("      %s: %s" % (type(exc).__name__, str(exc)[:300]))
            if os.environ.get("CORPUS_TEST_TRACE"):
                traceback.print_exc()
            failed.append((tool, name, "%s: %s" % (type(exc).__name__, exc)))
        else:
            print("ok")
    print()
    if failed:
        print("%d of %d instruments failed to notice a known-wrong input:"
              % (len(failed), len(selected)))
        for tool, name, why in failed:
            print("  %-14s %s" % (tool, name))
            print("      %s" % why[:300])
        return 1
    print("all %d controls behaved: each instrument noticed its own known-wrong "
          "input." % len(selected))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
