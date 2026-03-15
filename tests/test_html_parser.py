"""Tests for the HTML parser (html/parser.py)."""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest
from html.parser import parse
from html.dom import Document, Element, Text, Comment


def _find_tag(node, tag):
    """Return first descendant Element with the given tag name."""
    for child in node.children:
        if isinstance(child, Element) and child.tag == tag:
            return child
        found = _find_tag(child, tag)
        if found is not None:
            return found
    return None


def _find_all_tags(node, tag):
    """Return all descendant Elements with the given tag name."""
    results = []
    for child in node.children:
        if isinstance(child, Element) and child.tag == tag:
            results.append(child)
        results.extend(_find_all_tags(child, tag))
    return results


class TestBasicParsing(unittest.TestCase):
    def test_simple_element(self):
        doc = parse('<p>Hello</p>')
        self.assertIsInstance(doc, Document)
        p = _find_tag(doc, 'p')
        self.assertIsNotNone(p)
        self.assertEqual(p.tag, 'p')

    def test_nested_elements(self):
        doc = parse('<div><span>text</span></div>')
        div = _find_tag(doc, 'div')
        self.assertIsNotNone(div)
        span = _find_tag(div, 'span')
        self.assertIsNotNone(span)

    def test_attributes_double_quoted(self):
        doc = parse('<div id="main" class="container"></div>')
        div = _find_tag(doc, 'div')
        self.assertIsNotNone(div)
        self.assertEqual(div.attributes.get('id'), 'main')
        self.assertEqual(div.attributes.get('class'), 'container')

    def test_attributes_single_quoted(self):
        doc = parse("<img src='/a.png' alt='test' />")
        img = _find_tag(doc, 'img')
        self.assertIsNotNone(img)
        self.assertEqual(img.attributes.get('src'), '/a.png')
        self.assertEqual(img.attributes.get('alt'), 'test')

    def test_attributes_unquoted(self):
        doc = parse('<input type=text disabled>')
        inp = _find_tag(doc, 'input')
        self.assertIsNotNone(inp)
        self.assertEqual(inp.attributes.get('type'), 'text')
        self.assertIn('disabled', inp.attributes)

    def test_boolean_attribute(self):
        doc = parse('<input disabled>')
        inp = _find_tag(doc, 'input')
        self.assertIsNotNone(inp)
        self.assertIn('disabled', inp.attributes)

    def test_text_content(self):
        doc = parse('<p>Hello world</p>')
        p = _find_tag(doc, 'p')
        self.assertIsNotNone(p)
        texts = [c.data for c in p.children if isinstance(c, Text)]
        self.assertTrue(any('Hello world' in t for t in texts))

    def test_document_type(self):
        doc = parse('<!doctype html><html></html>')
        self.assertIsInstance(doc, Document)
        self.assertEqual(doc.type, 'ROOT')


class TestDocumentStructure(unittest.TestCase):
    def test_html_element_present(self):
        doc = parse('<html><body></body></html>')
        self.assertTrue(any(isinstance(c, Element) and c.tag == 'html' for c in doc.children))

    def test_implicit_html_body(self):
        """Parser should create implicit html/body elements for content."""
        doc = parse('<p>text</p>')
        html_el = _find_tag(doc, 'html')
        self.assertIsNotNone(html_el)
        body_el = _find_tag(doc, 'body')
        self.assertIsNotNone(body_el)
        p_el = _find_tag(doc, 'p')
        self.assertIsNotNone(p_el)

    def test_head_and_body(self):
        doc = parse('<html><head><title>T</title></head><body><p>P</p></body></html>')
        html_el = _find_tag(doc, 'html')
        self.assertIsNotNone(html_el)
        head = _find_tag(html_el, 'head')
        body = _find_tag(html_el, 'body')
        self.assertIsNotNone(head)
        self.assertIsNotNone(body)


class TestVoidElements(unittest.TestCase):
    def test_br_is_void(self):
        doc = parse('<p>line1<br>line2</p>')
        p = _find_tag(doc, 'p')
        br = _find_tag(p, 'br')
        self.assertIsNotNone(br)
        self.assertEqual(br.children, [])

    def test_img_is_void(self):
        doc = parse('<img src="x.png">')
        img = _find_tag(doc, 'img')
        self.assertIsNotNone(img)
        self.assertEqual(img.children, [])

    def test_input_is_void(self):
        doc = parse('<input type="text">')
        inp = _find_tag(doc, 'input')
        self.assertIsNotNone(inp)
        self.assertEqual(inp.children, [])

    def test_self_closing_syntax(self):
        doc = parse('<br />')
        br = _find_tag(doc, 'br')
        self.assertIsNotNone(br)


class TestComments(unittest.TestCase):
    def test_comment_parsed(self):
        doc = parse('<div><!-- a comment --></div>')
        div = _find_tag(doc, 'div')
        self.assertIsNotNone(div)
        comments = [c for c in div.children if isinstance(c, Comment)]
        self.assertTrue(len(comments) > 0)
        self.assertIn('a comment', comments[0].data)

    def test_comment_does_not_become_text(self):
        doc = parse('<p><!-- hidden -->visible</p>')
        p = _find_tag(doc, 'p')
        texts = [c.data for c in p.children if isinstance(c, Text)]
        self.assertTrue(any('visible' in t for t in texts))
        self.assertFalse(any('hidden' in t for t in texts))


class TestErrorRecovery(unittest.TestCase):
    def test_unclosed_tags(self):
        doc = parse('<div><span>hello<div>world')
        self.assertIsInstance(doc, Document)
        self.assertGreater(len(doc.children), 0)

    def test_extra_end_tags(self):
        doc = parse('<p>text</p></p></div>')
        self.assertIsInstance(doc, Document)
        p = _find_tag(doc, 'p')
        self.assertIsNotNone(p)

    def test_mismatched_tags(self):
        doc = parse('<b><i>text</b></i>')
        self.assertIsInstance(doc, Document)

    def test_empty_input(self):
        doc = parse('')
        self.assertIsInstance(doc, Document)

    def test_text_only(self):
        doc = parse('just some text')
        self.assertIsInstance(doc, Document)


class TestRawTextElements(unittest.TestCase):
    def test_script_content_preserved(self):
        doc = parse('<script>var x = 1 < 2;</script>')
        script = _find_tag(doc, 'script')
        self.assertIsNotNone(script)
        texts = [c.data for c in script.children if isinstance(c, Text)]
        content = ''.join(texts)
        self.assertIn('var x', content)

    def test_style_content_preserved(self):
        doc = parse('<style>.a > .b { color: red; }</style>')
        style = _find_tag(doc, 'style')
        self.assertIsNotNone(style)
        texts = [c.data for c in style.children if isinstance(c, Text)]
        content = ''.join(texts)
        self.assertIn('color: red', content)

    def test_style_tags_inside_script_are_text(self):
        doc = parse('<script>var html = "<div></div>";</script>')
        script = _find_tag(doc, 'script')
        self.assertIsNotNone(script)
        divs = _find_all_tags(script, 'div')
        self.assertEqual(divs, [])  # <div> inside script is raw text, not element


class TestParentPointers(unittest.TestCase):
    def test_parent_pointer_set(self):
        doc = parse('<div><p>text</p></div>')
        div = _find_tag(doc, 'div')
        self.assertIsNotNone(div)
        p = _find_tag(div, 'p')
        self.assertIsNotNone(p)
        self.assertIs(p.parent, div)

    def test_text_node_parent(self):
        doc = parse('<p>hello</p>')
        p = _find_tag(doc, 'p')
        self.assertIsNotNone(p)
        texts = [c for c in p.children if isinstance(c, Text)]
        self.assertTrue(len(texts) > 0)
        self.assertIs(texts[0].parent, p)


class TestComplexDocument(unittest.TestCase):
    def test_real_world_like_document(self):
        html = """<!doctype html>
<html lang="zh-CN">
<head>
    <meta charset="utf-8">
    <title>Test Page</title>
    <script>window.__STATE__ = {"items": [1, 2, 3]};</script>
    <style>.grid{display:grid;gap:12px;}</style>
</head>
<body>
    <header><nav><a href="/">Home</a></nav></header>
    <main id="app">
        <section class="grid">
            <article class="card"><h2>Title A</h2><p>Para A.</p></article>
        </section>
        <img src="hero.png">
        <input type="text" disabled>
        <br>
        <!-- analytics -->
    </main>
    <footer><small>copyright</small></footer>
</body>
</html>"""
        doc = parse(html)
        self.assertIsInstance(doc, Document)
        self.assertEqual(doc.type, 'ROOT')
        self.assertTrue(any(isinstance(c, Element) and c.tag == 'html' for c in doc.children))
        main = _find_tag(doc, 'main')
        self.assertIsNotNone(main)
        self.assertEqual(main.attributes.get('id'), 'app')
        h2 = _find_tag(doc, 'h2')
        self.assertIsNotNone(h2)


if __name__ == '__main__':
    unittest.main()
