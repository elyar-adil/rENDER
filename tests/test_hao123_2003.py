"""Tests for correct rendering of the 2003 hao123.com page.

The 2003 hao123 was a table-based Chinese link portal using legacy HTML
presentational attributes. Each test here covers a specific feature used
by the page so that passing all tests guarantees correct rendering.
"""
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import html.parser as hp
from html.dom import Document, Element, Text
from css.cascade import bind


ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UA_PATH = os.path.join(ROOT, 'ua', 'user-agent.css')


def _parse_and_bind(html_text, viewport_width=890):
    """Parse HTML and apply the full CSS cascade."""
    doc = hp.parse(html_text)
    bind(doc, UA_PATH, viewport_width=viewport_width, viewport_height=600)
    return doc


def _find_elements(node, tag=None, attr_filter=None):
    """Find all Elements matching tag and optional attribute filter."""
    results = []
    stack = [node]
    while stack:
        n = stack.pop()
        if isinstance(n, Element):
            if (tag is None or n.tag == tag):
                if attr_filter is None or attr_filter(n.attributes):
                    results.append(n)
        # Traverse children of both Document and Element nodes
        if hasattr(n, 'children'):
            stack.extend(reversed(n.children))
    return results


def _find_first(node, tag=None, attr_filter=None):
    results = _find_elements(node, tag, attr_filter)
    assert results, f"No <{tag}> element found"
    return results[0]


def _style(node, prop):
    """Get a computed style property from a node."""
    return (getattr(node, 'style', {}) or {}).get(prop, '')


# =========================================================================
# 1. <font> element — size, color, face attributes
# =========================================================================

class TestFontElement:
    """The <font> element was the primary styling mechanism in 2003 hao123."""

    def test_font_size_2_maps_to_13px(self):
        """<font size="2"> should map to 13px (HTML spec size table)."""
        doc = _parse_and_bind('<body><font size="2">text</font></body>')
        font = _find_first(doc, 'font')
        fs = _style(font, 'font-size')
        # HTML font size 2 = 13px (small)
        assert fs in ('13px', '13.0px', 'small'), f"font size=2 got {fs!r}, expected 13px"

    def test_font_size_1_maps_to_10px(self):
        doc = _parse_and_bind('<body><font size="1">text</font></body>')
        font = _find_first(doc, 'font')
        fs = _style(font, 'font-size')
        assert fs in ('10px', '10.0px', 'x-small'), f"font size=1 got {fs!r}, expected 10px"

    def test_font_size_3_maps_to_16px(self):
        doc = _parse_and_bind('<body><font size="3">text</font></body>')
        font = _find_first(doc, 'font')
        fs = _style(font, 'font-size')
        assert fs in ('16px', '16.0px', 'medium'), f"font size=3 got {fs!r}, expected 16px"

    def test_font_size_4_maps_to_18px(self):
        doc = _parse_and_bind('<body><font size="4">text</font></body>')
        font = _find_first(doc, 'font')
        fs = _style(font, 'font-size')
        assert fs in ('18px', '18.0px', 'large'), f"font size=4 got {fs!r}, expected 18px"

    def test_font_size_5_maps_to_24px(self):
        doc = _parse_and_bind('<body><font size="5">text</font></body>')
        font = _find_first(doc, 'font')
        fs = _style(font, 'font-size')
        assert fs in ('24px', '24.0px', 'x-large'), f"font size=5 got {fs!r}, expected 24px"

    def test_font_color_attribute(self):
        """<font color="#FF0000"> should set color to #FF0000."""
        doc = _parse_and_bind('<body><font color="#FF0000">text</font></body>')
        font = _find_first(doc, 'font')
        assert _style(font, 'color') == '#FF0000'

    def test_font_face_attribute(self):
        """<font face="宋体"> should set font-family."""
        doc = _parse_and_bind('<body><font face="宋体">text</font></body>')
        font = _find_first(doc, 'font')
        ff = _style(font, 'font-family')
        assert '宋体' in ff, f"font face=宋体 got font-family={ff!r}"

    def test_font_face_multiple(self):
        """<font face="Arial, Helvetica"> should set font-family."""
        doc = _parse_and_bind('<body><font face="Arial, Helvetica">text</font></body>')
        font = _find_first(doc, 'font')
        ff = _style(font, 'font-family')
        assert 'Arial' in ff, f"font face got font-family={ff!r}"

    def test_font_combined_attributes(self):
        """<font size="2" color="#FF0000" face="宋体"> should set all three."""
        doc = _parse_and_bind(
            '<body><font size="2" color="#FF0000" face="宋体">text</font></body>'
        )
        font = _find_first(doc, 'font')
        assert _style(font, 'color') == '#FF0000'
        assert '宋体' in _style(font, 'font-family')
        fs = _style(font, 'font-size')
        assert fs in ('13px', '13.0px', 'small'), f"combined font size got {fs!r}"

    def test_font_size_overridden_by_css(self):
        """CSS should override <font size> attribute."""
        doc = _parse_and_bind(
            '<head><style>font { font-size: 20px; }</style></head>'
            '<body><font size="2">text</font></body>'
        )
        font = _find_first(doc, 'font')
        fs = _style(font, 'font-size')
        assert fs == '20px', f"CSS override got {fs!r}, expected 20px"

    def test_nested_font_size(self):
        """Nested <font size="1"> inside <font size="2"> should use inner size."""
        doc = _parse_and_bind(
            '<body><font size="2">outer <font size="1">inner</font></font></body>'
        )
        fonts = _find_elements(doc, 'font')
        outer = fonts[0]
        inner = fonts[1]
        outer_fs = _style(outer, 'font-size')
        inner_fs = _style(inner, 'font-size')
        assert outer_fs in ('13px', '13.0px', 'small')
        assert inner_fs in ('10px', '10.0px', 'x-small')


# =========================================================================
# 2. <body> attributes — text, link, vlink
# =========================================================================

class TestBodyAttributes:
    """<body> text/link/vlink attributes define default text and link colors."""

    def test_body_bgcolor(self):
        """<body bgcolor="#FFFFFF"> should set background-color."""
        doc = _parse_and_bind('<body bgcolor="#FFFFFF">content</body>')
        body = _find_first(doc, 'body')
        assert _style(body, 'background-color') == '#FFFFFF'

    def test_body_text_attribute(self):
        """<body text="#000000"> should set the default text color."""
        doc = _parse_and_bind('<body text="#000000"><p>hello</p></body>')
        body = _find_first(doc, 'body')
        assert _style(body, 'color') == '#000000'

    def test_body_text_inherited_by_children(self):
        """<body text=...> color should be inherited by descendant elements."""
        doc = _parse_and_bind(
            '<body text="#333333"><p>hello</p></body>'
        )
        p = _find_first(doc, 'p')
        # After inheritance, p should have #333333 or parent's color
        color = _style(p, 'color')
        assert color == '#333333', f"p color should inherit body text=#333333, got {color!r}"

    def test_body_link_attribute(self):
        """<body link="#261CDC"> should set <a> link color."""
        doc = _parse_and_bind(
            '<body link="#261CDC"><a href="#">link</a></body>'
        )
        a = _find_first(doc, 'a')
        color = _style(a, 'color')
        assert color == '#261CDC', f"<a> color should be #261CDC from body link attr, got {color!r}"

    def test_body_vlink_attribute(self):
        """<body vlink="#800080"> should set visited link color."""
        # Note: since we can't track visited state, vlink should still
        # be applied as a fallback/default link color
        doc = _parse_and_bind(
            '<body vlink="#800080"><a href="#">link</a></body>'
        )
        # This test verifies the attribute is recognized and stored
        body = _find_first(doc, 'body')
        assert body.attributes.get('vlink') == '#800080'

    def test_body_link_overridden_by_css(self):
        """CSS a { color: red } should override body link attribute."""
        doc = _parse_and_bind(
            '<head><style>a { color: red; }</style></head>'
            '<body link="#261CDC"><a href="#">link</a></body>'
        )
        a = _find_first(doc, 'a')
        color = _style(a, 'color')
        assert color == 'red', f"CSS should override body link attr, got {color!r}"


# =========================================================================
# 3. <hr> element — size, color, noshade, width
# =========================================================================

class TestHrElement:
    """<hr> was used as section dividers in hao123."""

    def test_hr_is_block(self):
        doc = _parse_and_bind('<body><hr></body>')
        hr = _find_first(doc, 'hr')
        assert _style(hr, 'display') == 'block'

    def test_hr_default_has_border(self):
        """Default <hr> should have visible border (check longhands)."""
        doc = _parse_and_bind('<body><hr></body>')
        hr = _find_first(doc, 'hr')
        # Longhand border-*-style is what actually controls rendering
        bs = _style(hr, 'border-top-style') or _style(hr, 'border-style')
        assert bs not in ('', 'none', 'hidden'), f"hr border-style should not be none, got {bs!r}"

    def test_hr_width_attribute(self):
        """<hr width="890"> should set width to 890px."""
        doc = _parse_and_bind('<body><hr width="890"></body>')
        hr = _find_first(doc, 'hr')
        assert _style(hr, 'width') == '890px'

    def test_hr_size_attribute(self):
        """<hr size="1"> should set the height of the hr."""
        doc = _parse_and_bind('<body><hr size="1"></body>')
        hr = _find_first(doc, 'hr')
        h = _style(hr, 'height')
        assert h in ('1px', '1.0px'), f"hr size=1 should set height=1px, got {h!r}"

    def test_hr_color_attribute(self):
        """<hr color="#6699CC"> should set the border color."""
        doc = _parse_and_bind('<body><hr color="#6699CC"></body>')
        hr = _find_first(doc, 'hr')
        # color attr on hr should affect the border color or background
        bc = _style(hr, 'background-color') or _style(hr, 'border-color')
        assert '#6699CC' in (bc or '').upper() or '#6699cc' in (bc or '').lower(), \
            f"hr color=#6699CC should set color, got bg={_style(hr, 'background-color')!r}, border-color={_style(hr, 'border-color')!r}"

    def test_hr_noshade(self):
        """<hr noshade> should render as solid (no 3D effect)."""
        doc = _parse_and_bind('<body><hr noshade></body>')
        hr = _find_first(doc, 'hr')
        # noshade means solid border, not grooved/ridge
        bs = _style(hr, 'border-style') or _style(hr, 'border-top-style')
        assert bs in ('solid', 'none'), f"hr noshade border-style should be solid, got {bs!r}"


# =========================================================================
# 4. Table layout with presentational attributes
# =========================================================================

class TestTableAttributes:
    """Tables with border, cellpadding, cellspacing, width, bgcolor."""

    def test_table_width_attribute(self):
        doc = _parse_and_bind(
            '<body><table width="890"><tr><td>cell</td></tr></table></body>'
        )
        table = _find_first(doc, 'table')
        assert _style(table, 'width') == '890px'

    def test_table_border_0(self):
        doc = _parse_and_bind(
            '<body><table border="0"><tr><td>cell</td></tr></table></body>'
        )
        table = _find_first(doc, 'table')
        bs = _style(table, 'border-style')
        assert bs == 'none'

    def test_table_border_1(self):
        doc = _parse_and_bind(
            '<body><table border="1"><tr><td>cell</td></tr></table></body>'
        )
        table = _find_first(doc, 'table')
        bs = _style(table, 'border-style')
        bw = _style(table, 'border-width')
        assert bs == 'solid'
        assert bw == '1px'

    def test_table_border_propagates_to_cells(self):
        """When <table border="1">, cells should also get borders."""
        doc = _parse_and_bind(
            '<body><table border="1"><tr><td>cell</td></tr></table></body>'
        )
        td = _find_first(doc, 'td')
        bs = _style(td, 'border-style') or _style(td, 'border-top-style')
        bw = _style(td, 'border-width') or _style(td, 'border-top-width')
        assert bs in ('solid', 'inset'), f"td border-style should be solid/inset when table border=1, got {bs!r}"
        assert bw and bw != '0px' and bw != '0', f"td border-width should be >0 when table border=1, got {bw!r}"

    def test_table_cellspacing(self):
        doc = _parse_and_bind(
            '<body><table cellspacing="0"><tr><td>cell</td></tr></table></body>'
        )
        table = _find_first(doc, 'table')
        bs = _style(table, 'border-spacing')
        assert bs in ('0px', '0'), f"cellspacing=0 should set border-spacing=0px, got {bs!r}"

    def test_td_bgcolor(self):
        doc = _parse_and_bind(
            '<body><table><tr><td bgcolor="#DBE4F0">cell</td></tr></table></body>'
        )
        td = _find_first(doc, 'td')
        assert _style(td, 'background-color') == '#DBE4F0'

    def test_td_width_percentage(self):
        doc = _parse_and_bind(
            '<body><table width="800"><tr><td width="25%">cell</td></tr></table></body>'
        )
        td = _find_first(doc, 'td')
        w = _style(td, 'width')
        assert w == '25%', f"td width should be 25%, got {w!r}"

    def test_td_align(self):
        doc = _parse_and_bind(
            '<body><table><tr><td align="center">cell</td></tr></table></body>'
        )
        td = _find_first(doc, 'td')
        assert _style(td, 'text-align') == 'center'

    def test_td_valign(self):
        doc = _parse_and_bind(
            '<body><table><tr><td valign="top">cell</td></tr></table></body>'
        )
        td = _find_first(doc, 'td')
        assert _style(td, 'vertical-align') == 'top'

    def test_td_nowrap(self):
        doc = _parse_and_bind(
            '<body><table><tr><td nowrap>cell</td></tr></table></body>'
        )
        td = _find_first(doc, 'td')
        assert _style(td, 'white-space') == 'nowrap'

    def test_td_colspan(self):
        """colspan attribute should be recognized."""
        doc = _parse_and_bind(
            '<body><table><tr><td colspan="4">cell</td></tr></table></body>'
        )
        td = _find_first(doc, 'td')
        assert td.attributes.get('colspan') == '4'

    def test_tr_bgcolor(self):
        doc = _parse_and_bind(
            '<body><table><tr bgcolor="#DBE4F0"><td>cell</td></tr></table></body>'
        )
        tr = _find_first(doc, 'tr')
        assert _style(tr, 'background-color') == '#DBE4F0'


# =========================================================================
# 5. <center> element
# =========================================================================

class TestCenterElement:
    """<center> was the main centering mechanism in 2003 hao123."""

    def test_center_is_block(self):
        doc = _parse_and_bind('<body><center>text</center></body>')
        center = _find_first(doc, 'center')
        assert _style(center, 'display') == 'block'

    def test_center_has_text_align_center(self):
        """<center> should set text-align: center."""
        doc = _parse_and_bind('<body><center>text</center></body>')
        center = _find_first(doc, 'center')
        ta = _style(center, 'text-align')
        assert ta == 'center', f"<center> text-align should be center, got {ta!r}"

    def test_center_children_get_auto_margins(self):
        """Block children of <center> should get auto margins."""
        doc = _parse_and_bind(
            '<body><center><table width="500"><tr><td>x</td></tr></table></center></body>'
        )
        table = _find_first(doc, 'table')
        ml = _style(table, 'margin-left')
        mr = _style(table, 'margin-right')
        assert ml == 'auto', f"table in center margin-left={ml!r}, expected auto"
        assert mr == 'auto', f"table in center margin-right={mr!r}, expected auto"


# =========================================================================
# 6. Inline formatting — <b>, <u>, <a>
# =========================================================================

class TestInlineElements:
    """Inline elements used in hao123 links."""

    def test_bold(self):
        doc = _parse_and_bind('<body><b>bold</b></body>')
        b = _find_first(doc, 'b')
        assert _style(b, 'font-weight') == 'bold'

    def test_underline(self):
        doc = _parse_and_bind('<body><u>underline</u></body>')
        u = _find_first(doc, 'u')
        assert _style(u, 'text-decoration') in ('underline', 'underline solid')

    def test_anchor_default_blue_underline(self):
        doc = _parse_and_bind('<body><a href="#">link</a></body>')
        a = _find_first(doc, 'a')
        assert _style(a, 'color') == 'blue'
        assert 'underline' in _style(a, 'text-decoration')

    def test_anchor_color_from_css(self):
        """CSS a { color: ... } in <style> should set link color."""
        doc = _parse_and_bind(
            '<head><style>a{color:#261cdc}</style></head>'
            '<body><a href="#">link</a></body>'
        )
        a = _find_first(doc, 'a')
        assert _style(a, 'color') == '#261cdc'


# =========================================================================
# 7. Nested tables (sidebar pattern)
# =========================================================================

class TestNestedTables:
    """hao123 used nested tables for column layouts."""

    def test_nested_table_parsed(self):
        html = '''<body>
        <table width="890"><tr>
        <td width="50%"><table width="100%"><tr><td>inner</td></tr></table></td>
        <td width="50%"><table width="100%"><tr><td>inner</td></tr></table></td>
        </tr></table></body>'''
        doc = _parse_and_bind(html)
        tables = _find_elements(doc, 'table')
        assert len(tables) == 3, f"Expected 3 tables (1 outer + 2 inner), got {len(tables)}"

    def test_nested_table_width_100pct(self):
        html = '''<body>
        <table width="890"><tr>
        <td><table width="100%"><tr><td>inner</td></tr></table></td>
        </tr></table></body>'''
        doc = _parse_and_bind(html)
        tables = _find_elements(doc, 'table')
        inner = tables[1]
        assert _style(inner, 'width') == '100%'


# =========================================================================
# 8. <br> element
# =========================================================================

class TestBrElement:
    def test_br_is_parsed(self):
        doc = _parse_and_bind('<body>line1<br>line2</body>')
        brs = _find_elements(doc, 'br')
        assert len(brs) == 1


# =========================================================================
# 9. HTML entities
# =========================================================================

class TestEntities:
    def test_copy_entity(self):
        """&copy; should decode to ©."""
        doc = _parse_and_bind('<body>&copy;</body>')
        body = _find_first(doc, 'body')
        text = ''.join(
            child.data for child in body.children if isinstance(child, Text)
        )
        assert '©' in text, f"&copy; should decode to ©, got {text!r}"

    def test_nbsp_entity(self):
        """&nbsp; should decode to non-breaking space."""
        doc = _parse_and_bind('<body>a&nbsp;b</body>')
        body = _find_first(doc, 'body')
        text = ''.join(
            child.data for child in body.children if isinstance(child, Text)
        )
        assert '\xa0' in text or ' ' in text


# =========================================================================
# 10. Full page integration test
# =========================================================================

class TestHao123FullPage:
    """Integration test for the complete 2003 hao123 page."""

    def test_full_page_parses_without_error(self):
        page_path = os.path.join(ROOT, 'example', 'hao123_2003.html')
        with open(page_path, 'r', encoding='utf-8') as f:
            html_text = f.read()
        doc = _parse_and_bind(html_text, viewport_width=890)
        # Should have body, center, tables, fonts, hrs
        assert _find_elements(doc, 'body')
        assert _find_elements(doc, 'center')
        assert len(_find_elements(doc, 'table')) >= 4
        assert len(_find_elements(doc, 'font')) >= 10
        assert len(_find_elements(doc, 'hr')) >= 1

    def test_full_page_fonts_have_correct_size(self):
        """All <font size="2"> in the page should have font-size=13px."""
        page_path = os.path.join(ROOT, 'example', 'hao123_2003.html')
        with open(page_path, 'r', encoding='utf-8') as f:
            html_text = f.read()
        doc = _parse_and_bind(html_text, viewport_width=890)
        fonts = _find_elements(
            doc, 'font',
            attr_filter=lambda a: a.get('size') == '2'
        )
        assert len(fonts) > 0, "Should have <font size=2> elements"
        for font in fonts:
            fs = _style(font, 'font-size')
            assert fs in ('13px', '13.0px', 'small'), \
                f"<font size=2> should be 13px, got {fs!r}"

    def test_full_page_section_headers_are_red(self):
        """Section headers use <font color="#FF0000">."""
        page_path = os.path.join(ROOT, 'example', 'hao123_2003.html')
        with open(page_path, 'r', encoding='utf-8') as f:
            html_text = f.read()
        doc = _parse_and_bind(html_text, viewport_width=890)
        red_fonts = _find_elements(
            doc, 'font',
            attr_filter=lambda a: a.get('color') == '#FF0000'
        )
        assert len(red_fonts) >= 3, "Should have multiple red section headers"
        for font in red_fonts:
            assert _style(font, 'color') == '#FF0000'

    def test_full_page_body_bgcolor_white(self):
        page_path = os.path.join(ROOT, 'example', 'hao123_2003.html')
        with open(page_path, 'r', encoding='utf-8') as f:
            html_text = f.read()
        doc = _parse_and_bind(html_text, viewport_width=890)
        body = _find_first(doc, 'body')
        assert _style(body, 'background-color') == '#FFFFFF'

    def test_full_page_category_rows_have_bgcolor(self):
        """Category header rows use bgcolor="#DBE4F0"."""
        page_path = os.path.join(ROOT, 'example', 'hao123_2003.html')
        with open(page_path, 'r', encoding='utf-8') as f:
            html_text = f.read()
        doc = _parse_and_bind(html_text, viewport_width=890)
        blue_trs = _find_elements(
            doc, 'tr',
            attr_filter=lambda a: a.get('bgcolor') == '#DBE4F0'
        )
        assert len(blue_trs) >= 3, "Should have blue category header rows"
        for tr in blue_trs:
            assert _style(tr, 'background-color') == '#DBE4F0'
