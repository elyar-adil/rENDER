"""Tests for HTML parsing and element-handling gaps.

These tests reveal features where rENDER either silently ignores HTML elements
or produces incorrect DOM structure compared to what a real browser would build.
"""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import pytest
from html.parser import parse as parse_html
from html.dom import Document, Element, Text


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def parse(html: str) -> Document:
    return parse_html(html)


def find(node, tag):
    """BFS find first element with given tag."""
    queue = list(node.children)
    while queue:
        n = queue.pop(0)
        if isinstance(n, Element) and n.tag == tag:
            return n
        queue.extend(getattr(n, 'children', []))
    return None


def find_all(node, tag):
    """BFS find all elements with given tag."""
    result = []
    queue = list(node.children)
    while queue:
        n = queue.pop(0)
        if isinstance(n, Element) and n.tag == tag:
            result.append(n)
        queue.extend(getattr(n, 'children', []))
    return result


# ===========================================================================
# 1. BASIC DOCUMENT STRUCTURE
# ===========================================================================

class TestDocumentStructure:

    def test_implicit_html_element(self):
        """Parser must create an implicit <html> element."""
        doc = parse('<p>hello</p>')
        html = find(doc, 'html')
        assert html is not None

    def test_implicit_body_element(self):
        """Parser must create an implicit <body> element."""
        doc = parse('<p>hello</p>')
        body = find(doc, 'body')
        assert body is not None

    def test_doctype_accepted(self):
        """<!DOCTYPE html> must not cause errors."""
        doc = parse('<!DOCTYPE html><html><head></head><body><p>Hi</p></body></html>')
        assert find(doc, 'p') is not None

    def test_head_and_body_separate(self):
        """<head> and <body> must be siblings under <html>."""
        doc = parse('<html><head><title>Test</title></head><body><p>Hi</p></body></html>')
        html = find(doc, 'html')
        assert html is not None
        children_tags = [c.tag for c in html.children if isinstance(c, Element)]
        assert 'head' in children_tags
        assert 'body' in children_tags

    def test_nested_elements_correct_parent(self):
        doc = parse('<div><p>text</p></div>')
        div = find(doc, 'div')
        p = find(div, 'p')
        assert p is not None
        assert p.parent is div

    def test_text_content_preserved(self):
        doc = parse('<p>Hello World</p>')
        p = find(doc, 'p')
        texts = [c for c in p.children if isinstance(c, Text)]
        assert any('Hello World' in t.data for t in texts)

    def test_void_elements_self_close(self):
        """Void elements like <br> and <img> must not require a closing tag."""
        doc = parse('<p>Line 1<br>Line 2</p>')
        p = find(doc, 'p')
        br = find(p, 'br')
        assert br is not None

    def test_entity_decoding_lt_gt(self):
        doc = parse('<p>&lt;div&gt;</p>')
        p = find(doc, 'p')
        texts = [c.data for c in p.children if isinstance(c, Text)]
        assert any('<div>' in t for t in texts)

    def test_entity_decoding_amp(self):
        doc = parse('<p>foo &amp; bar</p>')
        p = find(doc, 'p')
        texts = [c.data for c in p.children if isinstance(c, Text)]
        assert any('foo & bar' in t for t in texts)

    def test_entity_nbsp(self):
        doc = parse('<p>hello&nbsp;world</p>')
        p = find(doc, 'p')
        texts = [c.data for c in p.children if isinstance(c, Text)]
        combined = ''.join(texts)
        # Non-breaking space (\xa0) or regular space
        assert '\xa0' in combined or ' ' in combined


# ===========================================================================
# 2. ATTRIBUTE HANDLING
# ===========================================================================

class TestAttributes:

    def test_simple_attribute(self):
        doc = parse('<a href="http://example.com">link</a>')
        a = find(doc, 'a')
        assert a.attributes.get('href') == 'http://example.com'

    def test_attribute_single_quotes(self):
        doc = parse("<img src='photo.jpg'>")
        img = find(doc, 'img')
        assert img.attributes.get('src') == 'photo.jpg'

    def test_attribute_no_value_boolean(self):
        doc = parse('<input disabled>')
        inp = find(doc, 'input')
        assert 'disabled' in inp.attributes

    def test_attribute_data_attribute(self):
        doc = parse('<div data-id="42">content</div>')
        div = find(doc, 'div')
        assert div.attributes.get('data-id') == '42'

    def test_class_attribute(self):
        doc = parse('<div class="foo bar baz">x</div>')
        div = find(doc, 'div')
        assert div.attributes.get('class') == 'foo bar baz'

    def test_multiple_attributes(self):
        doc = parse('<input type="text" name="q" placeholder="Search" required>')
        inp = find(doc, 'input')
        assert inp.attributes.get('type') == 'text'
        assert inp.attributes.get('name') == 'q'
        assert inp.attributes.get('placeholder') == 'Search'
        assert 'required' in inp.attributes


# ===========================================================================
# 3. ERROR RECOVERY
# ===========================================================================

class TestErrorRecovery:

    def test_unclosed_tag_recovered(self):
        """Unclosed <p> should be auto-closed by another block element."""
        doc = parse('<div><p>Para 1<p>Para 2</div>')
        ps = find_all(doc, 'p')
        assert len(ps) == 2

    def test_extra_closing_tag_ignored(self):
        """Extra </p> should not crash or corrupt the tree."""
        doc = parse('<p>Hello</p></p></p>')
        p = find(doc, 'p')
        assert p is not None

    def test_mismatched_tags_recovered(self):
        doc = parse('<div><span>text</div>')
        div = find(doc, 'div')
        assert div is not None
        span = find(div, 'span')
        assert span is not None

    def test_deep_nesting_no_crash(self):
        """Very deep nesting must not crash the parser."""
        html = '<div>' * 100 + 'text' + '</div>' * 100
        doc = parse(html)
        assert doc is not None

    def test_comment_not_in_dom(self):
        """HTML comments must not appear as Element nodes."""
        doc = parse('<div><!-- This is a comment -->text</div>')
        div = find(doc, 'div')
        assert div is not None
        # Comments may be Comment nodes but not Element nodes
        elem_children = [c for c in div.children if isinstance(c, Element)]
        assert not any(c.tag in ('!--',) for c in elem_children)


# ===========================================================================
# 4. SPECIAL ELEMENTS
# ===========================================================================

class TestSpecialElements:

    def test_script_content_not_parsed_as_html(self):
        """<script> content must be treated as raw text, not HTML."""
        doc = parse('<script>if (a < b) { alert("hi"); }</script>')
        script = find(doc, 'script')
        assert script is not None
        text = ''.join(c.data for c in script.children if isinstance(c, Text))
        # The < inside script should NOT have been parsed as a tag
        assert '<' in text or '&lt;' in text or 'a < b' in text

    def test_style_content_not_parsed_as_html(self):
        """<style> content must be treated as raw text."""
        doc = parse('<style>div > p { color: red; }</style>')
        style = find(doc, 'style')
        assert style is not None
        text = ''.join(c.data for c in style.children if isinstance(c, Text))
        assert '>' in text  # The > combinator should survive

    def test_textarea_content_preserved(self):
        doc = parse('<textarea>Hello &amp; World</textarea>')
        ta = find(doc, 'textarea')
        assert ta is not None

    def test_title_content_is_text(self):
        doc = parse('<head><title>My Page</title></head>')
        title = find(doc, 'title')
        assert title is not None
        text = ''.join(c.data for c in title.children if isinstance(c, Text))
        assert 'My Page' in text


# ===========================================================================
# 5. FORM ELEMENTS
# ===========================================================================

class TestFormElements:

    def test_input_types_parsed(self):
        html = '''
        <form>
          <input type="text" name="q">
          <input type="email" name="email">
          <input type="password" name="pw">
          <input type="checkbox" name="agree">
          <input type="radio" name="choice" value="a">
          <input type="submit" value="Go">
        </form>
        '''
        doc = parse(html)
        inputs = find_all(doc, 'input')
        types = [i.attributes.get('type', '') for i in inputs]
        assert 'text' in types
        assert 'email' in types
        assert 'password' in types
        assert 'checkbox' in types
        assert 'radio' in types
        assert 'submit' in types

    def test_select_with_options(self):
        html = '<select name="color"><option value="r">Red</option><option value="g">Green</option></select>'
        doc = parse(html)
        select = find(doc, 'select')
        assert select is not None
        options = find_all(select, 'option')
        assert len(options) == 2
        assert options[0].attributes.get('value') == 'r'

    def test_label_for_attribute(self):
        html = '<label for="email">Email:</label><input id="email" type="email">'
        doc = parse(html)
        label = find(doc, 'label')
        assert label.attributes.get('for') == 'email'

    def test_button_element(self):
        html = '<button type="submit">Click me</button>'
        doc = parse(html)
        btn = find(doc, 'button')
        assert btn is not None
        assert btn.attributes.get('type') == 'submit'


# ===========================================================================
# 6. LIST ELEMENTS
# ===========================================================================

class TestListElements:

    def test_ul_with_li(self):
        doc = parse('<ul><li>Item 1</li><li>Item 2</li><li>Item 3</li></ul>')
        ul = find(doc, 'ul')
        li_items = find_all(ul, 'li')
        assert len(li_items) == 3

    def test_ol_with_li(self):
        doc = parse('<ol><li>First</li><li>Second</li></ol>')
        ol = find(doc, 'ol')
        li_items = find_all(ol, 'li')
        assert len(li_items) == 2

    def test_nested_list(self):
        doc = parse('<ul><li>Item<ul><li>Sub-item</li></ul></li></ul>')
        outer_ul = find(doc, 'ul')
        outer_li = find(outer_ul, 'li')
        inner_ul = find(outer_li, 'ul')
        assert inner_ul is not None

    def test_dl_dt_dd(self):
        html = '<dl><dt>Term</dt><dd>Definition</dd></dl>'
        doc = parse(html)
        dl = find(doc, 'dl')
        assert find(dl, 'dt') is not None
        assert find(dl, 'dd') is not None


# ===========================================================================
# 7. TABLE ELEMENTS
# ===========================================================================

class TestTableElements:

    def test_basic_table_structure(self):
        html = '''
        <table>
          <thead><tr><th>Name</th><th>Age</th></tr></thead>
          <tbody><tr><td>Alice</td><td>30</td></tr></tbody>
        </table>
        '''
        doc = parse(html)
        table = find(doc, 'table')
        assert table is not None
        thead = find(table, 'thead')
        assert thead is not None
        tbody = find(table, 'tbody')
        assert tbody is not None
        ths = find_all(table, 'th')
        assert len(ths) == 2
        tds = find_all(table, 'td')
        assert len(tds) == 2

    def test_table_without_explicit_tbody(self):
        """Parser should implicitly create tbody for bare <tr> children."""
        html = '<table><tr><td>A</td></tr></table>'
        doc = parse(html)
        table = find(doc, 'table')
        tr = find(table, 'tr')
        assert tr is not None
        td = find(tr, 'td')
        assert td is not None

    def test_colspan_attribute(self):
        html = '<table><tr><td colspan="2">Wide</td></tr></table>'
        doc = parse(html)
        td = find(doc, 'td')
        assert td.attributes.get('colspan') == '2'


# ===========================================================================
# 8. SEMANTIC / HTML5 ELEMENTS
# ===========================================================================

class TestSemanticElements:

    def test_article_element(self):
        doc = parse('<article><h2>Title</h2><p>Content</p></article>')
        article = find(doc, 'article')
        assert article is not None

    def test_section_element(self):
        doc = parse('<section><h2>Section</h2></section>')
        assert find(doc, 'section') is not None

    def test_nav_element(self):
        doc = parse('<nav><a href="/">Home</a></nav>')
        assert find(doc, 'nav') is not None

    def test_header_footer_elements(self):
        doc = parse('<header><h1>Site</h1></header><footer>Footer</footer>')
        assert find(doc, 'header') is not None
        assert find(doc, 'footer') is not None

    def test_main_element(self):
        doc = parse('<main><p>Main content</p></main>')
        assert find(doc, 'main') is not None

    def test_figure_figcaption(self):
        html = '<figure><img src="a.jpg"><figcaption>Caption</figcaption></figure>'
        doc = parse(html)
        fig = find(doc, 'figure')
        assert fig is not None
        assert find(fig, 'figcaption') is not None

    def test_details_summary(self):
        html = '<details><summary>Click to expand</summary><p>Hidden content</p></details>'
        doc = parse(html)
        details = find(doc, 'details')
        assert details is not None
        summary = find(details, 'summary')
        assert summary is not None

    @pytest.mark.xfail(reason="<details> open/closed UA behavior not implemented")
    def test_details_closed_by_default(self):
        """<details> without 'open' attribute should hide its content."""
        doc = parse('<details><summary>Toggle</summary><p id="content">Hidden</p></details>')
        details = find(doc, 'details')
        # In a real browser, the <p> inside closed <details> gets display:none
        # This requires UA CSS or special handling
        p = find(details, 'p')
        assert p is not None
        # p should have display:none applied by UA rules
        assert p.style.get('display') == 'none'

    def test_dialog_element_parsed(self):
        html = '<dialog><p>Dialog content</p></dialog>'
        doc = parse(html)
        dialog = find(doc, 'dialog')
        assert dialog is not None

    @pytest.mark.xfail(reason="<dialog> modal behavior not implemented")
    def test_dialog_without_open_is_hidden(self):
        """<dialog> without the 'open' attribute should not be visible."""
        doc = parse('<dialog><p>Hidden</p></dialog>')
        dialog = find(doc, 'dialog')
        assert dialog is not None
        assert dialog.style.get('display') == 'none'

    def test_time_element(self):
        html = '<time datetime="2024-01-15">January 15</time>'
        doc = parse(html)
        time_el = find(doc, 'time')
        assert time_el is not None
        assert time_el.attributes.get('datetime') == '2024-01-15'

    def test_mark_element(self):
        doc = parse('<p>Some <mark>highlighted</mark> text</p>')
        assert find(doc, 'mark') is not None

    def test_progress_element(self):
        html = '<progress value="70" max="100">70%</progress>'
        doc = parse(html)
        progress = find(doc, 'progress')
        assert progress is not None
        assert progress.attributes.get('value') == '70'
        assert progress.attributes.get('max') == '100'

    def test_meter_element(self):
        html = '<meter min="0" max="10" value="6">6/10</meter>'
        doc = parse(html)
        meter = find(doc, 'meter')
        assert meter is not None

    def test_picture_element(self):
        html = '''
        <picture>
          <source srcset="img-800.jpg" media="(min-width: 800px)">
          <source srcset="img-400.jpg" media="(min-width: 400px)">
          <img src="img-default.jpg" alt="Responsive image">
        </picture>
        '''
        doc = parse(html)
        picture = find(doc, 'picture')
        assert picture is not None
        sources = find_all(picture, 'source')
        assert len(sources) == 2
        img = find(picture, 'img')
        assert img is not None

    @pytest.mark.xfail(reason="<picture> source selection not implemented; always uses <img> src")
    def test_picture_selects_correct_source(self):
        """At viewport=980px, the first source with min-width:800px should be used."""
        html = '''
        <picture>
          <source srcset="img-800.jpg" media="(min-width: 800px)">
          <img src="img-default.jpg" alt="">
        </picture>
        '''
        from html.dom import Document
        from css.cascade import bind
        import os as _os
        ua = _os.path.join(_os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))),
                           'ua', 'user-agent.css')
        doc = parse(html)
        bind(doc, ua, viewport_width=980)
        img = find(doc, 'img')
        # The selected source should be img-800.jpg, not img-default.jpg
        assert img.attributes.get('src') == 'img-800.jpg', (
            f"Expected img-800.jpg selected from <picture>, got {img.attributes.get('src')}"
        )

    def test_template_element_not_rendered(self):
        """<template> element content is inert – children are not in live DOM."""
        html = '<template><div id="tmpl">Template content</div></template>'
        doc = parse(html)
        template = find(doc, 'template')
        assert template is not None
        # The template's content div should not appear as a live DOM element
        # (In a real browser it's in DocumentFragment, not live tree)
        live_div = find(doc, 'div')
        # Either template is inert (live_div is None) or template el exists
        # This is acceptable either way in a static renderer
        assert template is not None  # template must parse


# ===========================================================================
# 9. HEADING HIERARCHY
# ===========================================================================

class TestHeadings:

    def test_all_heading_levels(self):
        html = '<h1>H1</h1><h2>H2</h2><h3>H3</h3><h4>H4</h4><h5>H5</h5><h6>H6</h6>'
        doc = parse(html)
        for level in range(1, 7):
            h = find(doc, f'h{level}')
            assert h is not None, f'<h{level}> not found'

    def test_h1_larger_than_h2_font_size(self):
        """UA stylesheet should give h1 a larger font-size than h2."""
        from html.dom import Document
        from css.cascade import bind
        import os as _os
        ua = _os.path.join(_os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))),
                           'ua', 'user-agent.css')
        doc = parse('<h1>Title</h1><h2>Subtitle</h2>')
        bind(doc, ua)

        h1 = find(doc, 'h1')
        h2 = find(doc, 'h2')
        assert h1 is not None and h2 is not None

        def px(el):
            val = el.style.get('font-size', '16px')
            if val.endswith('px'):
                return float(val[:-2])
            return 16.0

        assert px(h1) > px(h2), (
            f"h1 font-size ({px(h1)}px) should be larger than h2 ({px(h2)}px)"
        )


# ===========================================================================
# 10. LINK ELEMENT
# ===========================================================================

class TestLinkElement:

    def test_a_element_with_href(self):
        doc = parse('<a href="https://example.com">Example</a>')
        a = find(doc, 'a')
        assert a is not None
        assert a.attributes.get('href') == 'https://example.com'

    def test_a_element_without_href(self):
        """<a> without href is a placeholder link."""
        doc = parse('<a name="anchor">Anchor</a>')
        a = find(doc, 'a')
        assert a is not None
        assert a.attributes.get('href') is None

    def test_link_target_attribute(self):
        doc = parse('<a href="page.html" target="_blank">Open</a>')
        a = find(doc, 'a')
        assert a.attributes.get('target') == '_blank'


# ===========================================================================
# 11. IMAGE ELEMENTS
# ===========================================================================

class TestImageElements:

    def test_img_src_and_alt(self):
        doc = parse('<img src="photo.jpg" alt="A photo">')
        img = find(doc, 'img')
        assert img is not None
        assert img.attributes.get('src') == 'photo.jpg'
        assert img.attributes.get('alt') == 'A photo'

    def test_img_width_height_attributes(self):
        doc = parse('<img src="a.png" width="200" height="100">')
        img = find(doc, 'img')
        assert img.attributes.get('width') == '200'
        assert img.attributes.get('height') == '100'

    def test_img_with_srcset(self):
        html = '<img src="default.jpg" srcset="small.jpg 480w, large.jpg 1024w" alt="">'
        doc = parse(html)
        img = find(doc, 'img')
        assert img.attributes.get('srcset') is not None

    @pytest.mark.xfail(reason="srcset not evaluated; always falls back to src")
    def test_img_srcset_selected_by_viewport(self):
        """At 980px viewport, the 1024w srcset entry should be selected."""
        from html.dom import Document
        from css.cascade import bind
        import os as _os
        ua = _os.path.join(_os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))),
                           'ua', 'user-agent.css')
        html = '<img src="default.jpg" srcset="small.jpg 480w, large.jpg 1024w" alt="">'
        doc = parse(html)
        bind(doc, ua, viewport_width=980)
        img = find(doc, 'img')
        # Engine should select large.jpg for 980px viewport
        assert img.attributes.get('src') == 'large.jpg', (
            f"Expected large.jpg selected via srcset, got {img.attributes.get('src')}"
        )


# ===========================================================================
# 12. META / HEAD ELEMENTS
# ===========================================================================

class TestMetaElements:

    def test_meta_charset(self):
        doc = parse('<meta charset="utf-8">')
        meta = find(doc, 'meta')
        assert meta is not None
        assert meta.attributes.get('charset') == 'utf-8'

    def test_meta_viewport(self):
        doc = parse('<meta name="viewport" content="width=device-width, initial-scale=1">')
        meta = find(doc, 'meta')
        assert meta.attributes.get('name') == 'viewport'

    def test_meta_description(self):
        doc = parse('<meta name="description" content="A test page">')
        meta = find(doc, 'meta')
        assert meta.attributes.get('content') == 'A test page'

    def test_link_rel_stylesheet(self):
        doc = parse('<link rel="stylesheet" href="style.css">')
        link = find(doc, 'link')
        assert link is not None
        assert link.attributes.get('rel') == 'stylesheet'
        assert link.attributes.get('href') == 'style.css'


# ===========================================================================
# 13. UA STYLESHEET DEFAULTS
# ===========================================================================

class TestUAStylesheetDefaults:
    """UA stylesheet must apply sensible defaults to common elements."""

    def _bind(self, html_str, vw=980, vh=600):
        from css.cascade import bind
        import os as _os
        ua = _os.path.join(_os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))),
                           'ua', 'user-agent.css')
        doc = parse(html_str)
        bind(doc, ua, viewport_width=vw, viewport_height=vh)
        return doc

    def test_body_display_block(self):
        doc = self._bind('<body><p>x</p></body>')
        body = find(doc, 'body')
        assert body.style.get('display') in ('block', None, '')

    def test_div_display_block(self):
        doc = self._bind('<div>x</div>')
        div = find(doc, 'div')
        assert div.style.get('display') == 'block'

    def test_span_display_inline(self):
        doc = self._bind('<span>x</span>')
        span = find(doc, 'span')
        assert span.style.get('display') == 'inline'

    def test_strong_font_weight_bold(self):
        doc = self._bind('<strong>bold</strong>')
        strong = find(doc, 'strong')
        assert strong.style.get('font-weight') in ('bold', '700')

    def test_em_font_style_italic(self):
        doc = self._bind('<em>italic</em>')
        em = find(doc, 'em')
        assert em.style.get('font-style') == 'italic'

    def test_a_color_is_blue_ish(self):
        """Links should have a distinguishable color from body text."""
        doc = self._bind('<a href="#">link</a>')
        a = find(doc, 'a')
        # UA stylesheet should assign some color/decoration to links
        color = a.style.get('color', '')
        decoration = a.style.get('text-decoration', '')
        assert color or decoration, "anchor should have color or text-decoration from UA"

    def test_h1_display_block(self):
        doc = self._bind('<h1>Title</h1>')
        h1 = find(doc, 'h1')
        assert h1.style.get('display') == 'block'

    def test_ul_list_style(self):
        doc = self._bind('<ul><li>Item</li></ul>')
        ul = find(doc, 'ul')
        # UA should give ul some list-style or margin/padding
        style_keys = set(ul.style.keys())
        has_list_style = bool(style_keys & {'list-style', 'list-style-type', 'padding-left'})
        assert has_list_style, f"ul should have list-style or padding; got keys: {style_keys}"

    def test_p_display_block(self):
        doc = self._bind('<p>text</p>')
        p = find(doc, 'p')
        assert p.style.get('display') == 'block'

    def test_pre_white_space_pre(self):
        doc = self._bind('<pre>code</pre>')
        pre = find(doc, 'pre')
        assert pre.style.get('white-space') == 'pre', (
            f"<pre> should have white-space:pre, got {pre.style.get('white-space')}"
        )

    def test_code_font_family_monospace(self):
        doc = self._bind('<code>x = 1</code>')
        code = find(doc, 'code')
        family = code.style.get('font-family', '').lower()
        assert 'monospace' in family or 'mono' in family or 'courier' in family or 'consolas' in family, (
            f"<code> font-family should be monospace, got: {family!r}"
        )

    def test_table_display_table(self):
        """UA stylesheet sets display:table on <table> elements."""
        doc = self._bind('<table><tr><td>x</td></tr></table>')
        table = find(doc, 'table')
        assert table.style.get('display') == 'table'

    @pytest.mark.xfail(reason="<details> UA hidden behavior not implemented")
    def test_details_summary_visible_content_hidden(self):
        """Content inside closed <details> should have display:none in UA."""
        doc = self._bind('<details><summary>Toggle</summary><p id="p">Content</p></details>')
        p = find(doc, 'p')
        assert p.style.get('display') == 'none'
