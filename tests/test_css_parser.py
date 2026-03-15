"""Tests for CSS parser and selector matching."""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest
from css.parser import parse_stylesheet, parse_inline_style
from css.selector import matches, specificity
from html.dom import Element, Document


class TestCSSParser(unittest.TestCase):
    def test_parse_simple_rule(self):
        css = 'div { color: red; font-size: 14px; }'
        sheet = parse_stylesheet(css)
        self.assertEqual(len(sheet.rules), 1)
        rule = sheet.rules[0]
        self.assertIn('div', rule.prelude)
        props = {d.property: d.value for d in rule.declarations}
        self.assertEqual(props.get('color'), 'red')
        self.assertEqual(props.get('font-size'), '14px')

    def test_parse_multiple_rules(self):
        css = 'h1 { color: blue; } p { margin: 10px; }'
        sheet = parse_stylesheet(css)
        self.assertEqual(len(sheet.rules), 2)

    def test_parse_inline_style(self):
        result = parse_inline_style('color: red; font-size: 16px')
        self.assertEqual(result.get('color'), 'red')
        self.assertEqual(result.get('font-size'), '16px')

    def test_important_flag(self):
        css = 'p { color: red !important; }'
        sheet = parse_stylesheet(css)
        decl = sheet.rules[0].declarations[0]
        self.assertTrue(decl.important)

    def test_parse_class_selector(self):
        css = '.foo { color: green; }'
        sheet = parse_stylesheet(css)
        self.assertEqual(len(sheet.rules), 1)
        self.assertIn('.foo', sheet.rules[0].prelude)


class TestSelectorMatching(unittest.TestCase):
    def _make_element(self, tag, attrs=None):
        return Element(tag, attrs or {})

    def test_tag_selector(self):
        el = self._make_element('div')
        self.assertTrue(matches(el, 'div'))
        self.assertFalse(matches(el, 'p'))

    def test_class_selector(self):
        el = self._make_element('div', {'class': 'foo bar'})
        self.assertTrue(matches(el, '.foo'))
        self.assertTrue(matches(el, '.bar'))
        self.assertFalse(matches(el, '.baz'))

    def test_id_selector(self):
        el = self._make_element('div', {'id': 'main'})
        self.assertTrue(matches(el, '#main'))
        self.assertFalse(matches(el, '#other'))

    def test_universal_selector(self):
        el = self._make_element('span')
        self.assertTrue(matches(el, '*'))

    def test_specificity(self):
        a, b, c = specificity('#id')
        self.assertEqual(a, 1)
        a, b, c = specificity('.cls')
        self.assertEqual(b, 1)
        a, b, c = specificity('div')
        self.assertEqual(c, 1)


if __name__ == '__main__':
    unittest.main()
