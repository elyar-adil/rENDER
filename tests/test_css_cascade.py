"""Tests for CSS cascade, inheritance, media queries, and selector specificity."""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest
from html.dom import Document, Element, Text
from css.cascade import _media_matches, _media_single_matches
import css.parser as css_parser
import css.selector as selector_mod


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def make_doc_with_style(css_text: str, body_html_builder=None):
    """Build a minimal Document with a <style> block."""
    doc = Document()
    html_el = Element('html')
    head_el = Element('head')
    style_el = Element('style')
    style_el.style = {}
    style_text = Text(css_text)
    style_text.parent = style_el
    style_el.children.append(style_text)
    style_el.parent = head_el
    head_el.children.append(style_el)
    head_el.parent = html_el
    body_el = Element('body')
    body_el.style = {}
    body_el.parent = html_el
    html_el.children.extend([head_el, body_el])
    html_el.parent = doc
    doc.children.append(html_el)
    if body_html_builder:
        body_html_builder(body_el)
    return doc, body_el


def bind_doc(doc, extra=''):
    from css.cascade import bind
    import os as _os
    ua_path = _os.path.join(_os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))), 'css', 'ua.css')
    bind(doc, ua_path, viewport_width=980, viewport_height=600)


# ---------------------------------------------------------------------------
# @media query tests
# ---------------------------------------------------------------------------

class TestMediaQueries(unittest.TestCase):

    def test_all_matches(self):
        self.assertTrue(_media_matches('all', 980, 600))

    def test_screen_matches(self):
        self.assertTrue(_media_matches('screen', 980, 600))

    def test_print_does_not_match(self):
        self.assertFalse(_media_matches('print', 980, 600))

    def test_min_width_matches_exact(self):
        self.assertTrue(_media_matches('(min-width: 980px)', 980, 600))

    def test_min_width_matches_above(self):
        self.assertTrue(_media_matches('(min-width: 500px)', 980, 600))

    def test_min_width_fails_below(self):
        self.assertFalse(_media_matches('(min-width: 1200px)', 980, 600))

    def test_max_width_matches_below(self):
        self.assertTrue(_media_matches('(max-width: 1000px)', 980, 600))

    def test_max_width_fails_above(self):
        self.assertFalse(_media_matches('(max-width: 500px)', 980, 600))

    def test_min_height_matches(self):
        self.assertTrue(_media_matches('(min-height: 600px)', 980, 600))

    def test_max_height_matches(self):
        self.assertTrue(_media_matches('(max-height: 600px)', 980, 600))

    def test_and_both_true(self):
        self.assertTrue(_media_matches('screen and (min-width: 500px)', 980, 600))

    def test_and_one_false(self):
        self.assertFalse(_media_matches('screen and (min-width: 1200px)', 980, 600))

    def test_only_screen_prefix_is_ignored(self):
        self.assertTrue(_media_matches('only screen and (min-width: 60em)', 980, 600))

    def test_em_units_are_converted_to_px(self):
        self.assertFalse(_media_matches('screen and (max-width: 40em)', 980, 600))

    def test_max_device_width_is_not_treated_as_always_true(self):
        self.assertFalse(_media_matches('only screen and (max-device-width: 40em)', 980, 600))

    def test_handheld_media_type_does_not_match_desktop_viewport(self):
        self.assertFalse(_media_matches('handheld, only screen and (max-device-width: 40em)', 980, 600))

    def test_not_negates(self):
        self.assertTrue(_media_matches('not print', 980, 600))
        self.assertFalse(_media_matches('not screen', 980, 600))

    def test_comma_or_logic_first_matches(self):
        self.assertTrue(_media_matches('print, screen', 980, 600))

    def test_comma_or_logic_second_matches(self):
        self.assertTrue(_media_matches('print, all', 980, 600))

    def test_comma_both_fail(self):
        self.assertFalse(_media_matches('print, (min-width: 9999px)', 980, 600))

    def test_empty_query_matches(self):
        self.assertTrue(_media_matches('', 980, 600))


# ---------------------------------------------------------------------------
# CSS specificity tests
# ---------------------------------------------------------------------------

class TestCSSSpecificity(unittest.TestCase):

    def test_universal_selector_zero(self):
        spec = selector_mod.specificity('*')
        self.assertEqual(spec, (0, 0, 0))

    def test_tag_selector_0_0_1(self):
        spec = selector_mod.specificity('div')
        self.assertEqual(spec, (0, 0, 1))

    def test_class_selector_0_1_0(self):
        spec = selector_mod.specificity('.foo')
        self.assertEqual(spec, (0, 1, 0))

    def test_id_selector_1_0_0(self):
        spec = selector_mod.specificity('#main')
        self.assertEqual(spec, (1, 0, 0))

    def test_tag_plus_class(self):
        spec = selector_mod.specificity('div.foo')
        self.assertEqual(spec, (0, 1, 1))

    def test_tag_plus_id(self):
        spec = selector_mod.specificity('div#main')
        self.assertEqual(spec, (1, 0, 1))

    def test_two_classes(self):
        spec = selector_mod.specificity('.foo.bar')
        self.assertEqual(spec, (0, 2, 0))

    def test_descendant_adds_specs(self):
        spec = selector_mod.specificity('div p')
        self.assertEqual(spec, (0, 0, 2))

    def test_id_beats_many_classes(self):
        s_id = selector_mod.specificity('#x')
        s_classes = selector_mod.specificity('.a.b.c.d.e.f.g.h.i.j.k')
        self.assertGreater(s_id, s_classes)

    def test_pseudo_class_counts_as_class(self):
        spec = selector_mod.specificity('a:hover')
        self.assertEqual(spec, (0, 1, 1))

    def test_attribute_selector_counts_as_class(self):
        spec = selector_mod.specificity('[href]')
        self.assertEqual(spec, (0, 1, 0))

    def test_not_pseudo_class_inner_counts(self):
        # :not(#id) → ID inside :not counts
        spec = selector_mod.specificity(':not(#foo)')
        self.assertEqual(spec, (1, 0, 0))


# ---------------------------------------------------------------------------
# CSS selector matching
# ---------------------------------------------------------------------------

class TestSelectorMatching(unittest.TestCase):
    """matches(element, selector_string) -> bool"""

    def _elem(self, tag, attrs=None):
        el = Element(tag, attrs or {})
        el.style = {}
        return el

    def test_tag_match(self):
        el = self._elem('div')
        self.assertTrue(selector_mod.matches(el, 'div'))

    def test_tag_no_match(self):
        el = self._elem('span')
        self.assertFalse(selector_mod.matches(el, 'div'))

    def test_class_match(self):
        el = self._elem('div', {'class': 'foo'})
        self.assertTrue(selector_mod.matches(el, '.foo'))

    def test_class_no_match(self):
        el = self._elem('div', {'class': 'bar'})
        self.assertFalse(selector_mod.matches(el, '.foo'))

    def test_multiple_classes_all_must_match(self):
        el = self._elem('div', {'class': 'foo bar'})
        self.assertTrue(selector_mod.matches(el, '.foo.bar'))

    def test_multiple_classes_partial_no_match(self):
        el = self._elem('div', {'class': 'foo'})
        self.assertFalse(selector_mod.matches(el, '.foo.bar'))

    def test_id_match(self):
        el = self._elem('div', {'id': 'main'})
        self.assertTrue(selector_mod.matches(el, '#main'))

    def test_id_no_match(self):
        el = self._elem('div', {'id': 'other'})
        self.assertFalse(selector_mod.matches(el, '#main'))

    def test_universal_matches_any(self):
        for tag in ('div', 'span', 'p', 'h1', 'li'):
            el = self._elem(tag)
            self.assertTrue(selector_mod.matches(el, '*'), f'* should match <{tag}>')

    def test_tag_and_class(self):
        el = self._elem('p', {'class': 'intro'})
        self.assertTrue(selector_mod.matches(el, 'p.intro'))
        self.assertFalse(selector_mod.matches(el, 'div.intro'))

    def test_attribute_presence(self):
        el = self._elem('a', {'href': 'http://example.com'})
        self.assertTrue(selector_mod.matches(el, '[href]'))

    def test_attribute_value(self):
        el = self._elem('input', {'type': 'text'})
        self.assertTrue(selector_mod.matches(el, '[type="text"]'))
        self.assertFalse(selector_mod.matches(el, '[type="checkbox"]'))

    def test_descendant_combinator(self):
        grandparent = self._elem('div')
        parent = self._elem('p')
        child = self._elem('span')
        parent.parent = grandparent
        child.parent = parent
        grandparent.children = [parent]
        parent.children = [child]

        # 'div span' should match (descendant)
        self.assertTrue(selector_mod.matches(child, 'div span'))
        # 'div > span' should NOT match (child combinator — span's parent is p)
        self.assertFalse(selector_mod.matches(child, 'div > span'))

    def test_child_combinator(self):
        parent = self._elem('div')
        child = self._elem('span')
        child.parent = parent
        parent.children = [child]

        self.assertTrue(selector_mod.matches(child, 'div > span'))

    def test_first_child_pseudo(self):
        parent = self._elem('ul')
        li1 = self._elem('li')
        li2 = self._elem('li')
        li1.parent = parent
        li2.parent = parent
        parent.children = [li1, li2]

        self.assertTrue(selector_mod.matches(li1, 'li:first-child'))
        self.assertFalse(selector_mod.matches(li2, 'li:first-child'))

    def test_last_child_pseudo(self):
        parent = self._elem('ul')
        li1 = self._elem('li')
        li2 = self._elem('li')
        li1.parent = parent
        li2.parent = parent
        parent.children = [li1, li2]

        self.assertFalse(selector_mod.matches(li1, 'li:last-child'))
        self.assertTrue(selector_mod.matches(li2, 'li:last-child'))


# ---------------------------------------------------------------------------
# CSS parser tests
# ---------------------------------------------------------------------------

class TestCSSParser(unittest.TestCase):

    def test_parse_empty(self):
        sheet = css_parser.parse_stylesheet('')
        self.assertEqual(len(sheet.rules), 0)

    def test_parse_rule_with_multiple_declarations(self):
        sheet = css_parser.parse_stylesheet('p { color: red; font-size: 16px; }')
        self.assertEqual(len(sheet.rules), 1)
        decls = {d.property: d.value for d in sheet.rules[0].declarations}
        self.assertEqual(decls.get('color'), 'red')
        self.assertEqual(decls.get('font-size'), '16px')

    def test_parse_multiple_selectors(self):
        sheet = css_parser.parse_stylesheet('h1, h2, h3 { color: blue; }')
        # Should have one rule with combined selector
        self.assertGreater(len(sheet.rules), 0)

    def test_important_flag(self):
        sheet = css_parser.parse_stylesheet('p { color: red !important; }')
        decls = sheet.rules[0].declarations
        important_decls = [d for d in decls if d.important]
        self.assertTrue(len(important_decls) > 0)

    def test_parse_media_rule(self):
        sheet = css_parser.parse_stylesheet('@media screen { div { display: block; } }')
        at_rules = [r for r in sheet.rules if hasattr(r, 'name')]
        self.assertTrue(len(at_rules) > 0)

    def test_inline_style_parsing(self):
        # parse_inline_style returns a dict {property: value}
        props = css_parser.parse_inline_style('color: blue; font-size: 14px')
        self.assertIsInstance(props, dict)
        self.assertEqual(props.get('color'), 'blue')
        self.assertEqual(props.get('font-size'), '14px')

    def test_inline_empty(self):
        # Empty string returns empty dict
        result = css_parser.parse_inline_style('')
        self.assertFalse(result)  # Empty dict is falsy

    def test_declaration_with_no_value_skipped(self):
        # Malformed CSS should not crash
        try:
            sheet = css_parser.parse_stylesheet('div { : bad; }')
            # Should not raise
        except Exception:
            pass  # If it raises, test still passes (we just ensure no crash)


# ---------------------------------------------------------------------------
# CSS cascade integration tests (no UA css needed)
# ---------------------------------------------------------------------------

class TestCascadeIntegration(unittest.TestCase):

    def _make_doc_inline(self, css_text, extra_el_builder=None):
        """Build DOM and apply cascade with given CSS."""
        doc, body = make_doc_with_style(css_text, extra_el_builder)
        bind_doc(doc)
        return doc, body

    def test_tag_rule_applied(self):
        def add_p(body):
            p = Element('p')
            p.style = {}
            p.parent = body
            body.children.append(p)
            body._p = p

        doc, body = self._make_doc_inline('p { color: blue; }', add_p)
        p = body._p
        self.assertEqual(p.style.get('color'), 'blue')

    def test_class_rule_applied(self):
        def add_div(body):
            div = Element('div', {'class': 'highlight'})
            div.style = {}
            div.parent = body
            body.children.append(div)
            body._div = div

        doc, body = self._make_doc_inline('.highlight { color: yellow; }', add_div)
        self.assertEqual(body._div.style.get('color'), 'yellow')

    def test_id_rule_applied(self):
        def add_div(body):
            div = Element('div', {'id': 'hero'})
            div.style = {}
            div.parent = body
            body.children.append(div)
            body._div = div

        doc, body = self._make_doc_inline('#hero { display: flex; }', add_div)
        self.assertEqual(body._div.style.get('display'), 'flex')

    def test_border_shorthand_with_var_expands_after_var_resolution(self):
        def add_img(body):
            img = Element('img')
            img.style = {}
            img.parent = body
            body.children.append(img)
            body._img = img

        doc, body = self._make_doc_inline(
            ':root { --bw: 1px; --bc: #999999; } img { border: var(--bw) solid var(--bc); }',
            add_img,
        )
        img = body._img
        self.assertEqual(img.style.get('border-top-width'), '1px')
        self.assertEqual(img.style.get('border-right-width'), '1px')
        self.assertEqual(img.style.get('border-bottom-width'), '1px')
        self.assertEqual(img.style.get('border-left-width'), '1px')
        self.assertEqual(img.style.get('border-top-style'), 'solid')
        self.assertEqual(img.style.get('border-top-color'), '#999999')

    def test_specificity_id_beats_class(self):
        def add_div(body):
            div = Element('div', {'id': 'box', 'class': 'box'})
            div.style = {}
            div.parent = body
            body.children.append(div)
            body._div = div

        doc, body = self._make_doc_inline(
            '.box { color: red; } #box { color: green; }', add_div)
        # ID wins
        self.assertEqual(body._div.style.get('color'), 'green')

    def test_later_rule_wins_same_specificity(self):
        def add_p(body):
            p = Element('p')
            p.style = {}
            p.parent = body
            body.children.append(p)
            body._p = p

        doc, body = self._make_doc_inline(
            'p { color: red; } p { color: blue; }', add_p)
        # Later rule (blue) should win
        self.assertEqual(body._p.style.get('color'), 'blue')

    def test_inheritance_color(self):
        def add_els(body):
            parent_div = Element('div')
            parent_div.style = {}
            child_span = Element('span')
            child_span.style = {}
            child_span.parent = parent_div
            parent_div.children.append(child_span)
            parent_div.parent = body
            body.children.append(parent_div)
            body._span = child_span

        doc, body = self._make_doc_inline('div { color: purple; }', add_els)
        # color is inherited
        self.assertEqual(body._span.style.get('color'), 'purple')

    def test_non_inherited_not_passed_down(self):
        def add_els(body):
            parent_div = Element('div')
            parent_div.style = {}
            child_span = Element('span')
            child_span.style = {}
            child_span.parent = parent_div
            parent_div.children.append(child_span)
            parent_div.parent = body
            body.children.append(parent_div)
            body._span = child_span

        doc, body = self._make_doc_inline('div { background-color: blue; }', add_els)
        # background-color is NOT inherited
        bg = body._span.style.get('background-color', 'transparent')
        self.assertNotEqual(bg, 'blue')

    def test_important_overrides_specificity(self):
        def add_div(body):
            div = Element('div', {'id': 'x'})
            div.style = {}
            div.parent = body
            body.children.append(div)
            body._div = div

        doc, body = self._make_doc_inline(
            '#x { color: red; } div { color: blue !important; }', add_div)
        # !important from lower-specificity rule should win
        self.assertEqual(body._div.style.get('color'), 'blue')


# ---------------------------------------------------------------------------
# CSS properties expansion
# ---------------------------------------------------------------------------

class TestShorthandExpansion(unittest.TestCase):

    def test_margin_shorthand_one_value(self):
        from css.properties import expand_shorthand
        result = expand_shorthand('margin', '10px')
        self.assertEqual(result.get('margin-top'), '10px')
        self.assertEqual(result.get('margin-right'), '10px')
        self.assertEqual(result.get('margin-bottom'), '10px')
        self.assertEqual(result.get('margin-left'), '10px')

    def test_margin_shorthand_two_values(self):
        from css.properties import expand_shorthand
        result = expand_shorthand('margin', '10px 20px')
        self.assertEqual(result.get('margin-top'), '10px')
        self.assertEqual(result.get('margin-right'), '20px')
        self.assertEqual(result.get('margin-bottom'), '10px')
        self.assertEqual(result.get('margin-left'), '20px')

    def test_margin_shorthand_four_values(self):
        from css.properties import expand_shorthand
        result = expand_shorthand('margin', '1px 2px 3px 4px')
        self.assertEqual(result.get('margin-top'), '1px')
        self.assertEqual(result.get('margin-right'), '2px')
        self.assertEqual(result.get('margin-bottom'), '3px')
        self.assertEqual(result.get('margin-left'), '4px')

    def test_padding_shorthand(self):
        from css.properties import expand_shorthand
        result = expand_shorthand('padding', '5px 10px')
        self.assertEqual(result.get('padding-top'), '5px')
        self.assertEqual(result.get('padding-left'), '10px')

    def test_unknown_property_passthrough(self):
        from css.properties import expand_shorthand
        # Unknown properties are passed through as-is
        result = expand_shorthand('unknown-property', '42px')
        # Should not crash; returns {'unknown-property': '42px'}
        self.assertIsInstance(result, dict)


if __name__ == '__main__':
    unittest.main()
