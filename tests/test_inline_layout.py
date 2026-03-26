"""Tests for inline layout regressions."""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest

from html.dom import Element, Text
from layout.inline import layout_inline, _measure_inline_block_intrinsic_width
from layout.text import resolve_font_family


def _append(parent, child):
    child.parent = parent
    parent.children.append(child)
    return child


class TestInlineLayout(unittest.TestCase):
    def test_block_img_inside_inline_content_is_still_collected(self):
        root = Element('div')
        root.style = {'display': 'block', 'font-size': '16px'}

        link = _append(root, Element('a'))
        link.style = {'display': 'inline'}

        img = _append(link, Element('img', {'width': '18', 'height': '18'}))
        img.style = {'display': 'block'}
        img.natural_width = 18
        img.natural_height = 18

        lines, _ = layout_inline(root, 0.0, 0.0, 200.0)

        items = [item for line in lines for item in line.items]
        self.assertEqual(len(items), 1)
        self.assertEqual(items[0].type, 'IMG')
        self.assertEqual(items[0].width, 18.0)
        self.assertEqual(items[0].height, 18.0)

    def test_block_img_direct_child_is_not_collected_into_inline_flow(self):
        root = Element('div')
        root.style = {'display': 'block', 'font-size': '16px'}

        img = _append(root, Element('img', {'width': '18', 'height': '18'}))
        img.style = {'display': 'block'}
        img.natural_width = 18
        img.natural_height = 18

        link = _append(root, Element('a'))
        link.style = {'display': 'inline'}
        _append(link, Text('Read more'))

        lines, _ = layout_inline(root, 0.0, 0.0, 200.0)

        items = [item for line in lines for item in line.items]
        word_items = [item for item in items if item.type not in ('SPACE',)]
        self.assertEqual([item.type for item in word_items], ['WORD', 'WORD'])
        self.assertEqual([item.text for item in word_items], ['Read', 'more'])

    def test_text_input_is_emitted_as_inline_control(self):
        root = Element('div')
        root.style = {'display': 'block', 'font-size': '13px'}
        _append(root, Text('Search:'))

        inp = _append(root, Element('input', {'type': 'text', 'size': '17'}))
        inp.style = {'display': 'inline', 'font-size': '13px'}

        lines, _ = layout_inline(root, 0.0, 0.0, 400.0)

        items = [item for line in lines for item in line.items]
        controls = [item for item in items if item.control_type == 'text-input']
        self.assertEqual(len(controls), 1)
        self.assertGreater(controls[0].width, 100.0)
        self.assertGreater(controls[0].height, 10.0)

    def test_empty_block_icon_is_promoted_into_inline_flow(self):
        root = Element('div')
        root.style = {'display': 'block'}

        link = _append(root, Element('a'))
        link.style = {'display': 'inline'}

        icon = _append(link, Element('div'))
        icon.style = {
            'display': 'block',
            'width': '10px',
            'height': '10px',
            'background-color': '#ff6600',
        }

        lines, _ = layout_inline(root, 0.0, 0.0, 200.0)

        items = [item for line in lines for item in line.items]
        self.assertEqual(len(items), 1)
        self.assertEqual(items[0].type, 'INLINE-BLOCK')
        self.assertEqual(items[0].width, 10.0)
        self.assertEqual(items[0].height, 10.0)

    def test_plain_block_text_child_is_not_promoted_into_inline_flow(self):
        root = Element('div')
        root.style = {'display': 'block'}

        para = _append(root, Element('p'))
        para.style = {'display': 'block'}
        _append(para, Text('content'))

        link = _append(root, Element('a'))
        link.style = {'display': 'inline'}
        _append(link, Text('Read more'))

        lines, _ = layout_inline(root, 0.0, 0.0, 300.0)
        items = [item for line in lines for item in line.items]
        word_items = [item for item in items if item.type not in ('SPACE',)]
        self.assertEqual([item.type for item in word_items], ['WORD', 'WORD'])
        self.assertEqual([item.text for item in word_items], ['Read', 'more'])

    def test_resolve_font_family_returns_installed_family(self):
        from PyQt6.QtGui import QFontDatabase

        resolved = resolve_font_family('Verdana, Arial, sans-serif')
        available = set(QFontDatabase.families())

        self.assertTrue(available)
        self.assertTrue(resolved)
        self.assertIn(resolved, available)

    def test_intrinsic_width_counts_input_and_explicit_width(self):
        form = Element('form')
        form.style = {'display': 'inline-block'}

        input_el = _append(form, Element('input', {'type': 'text'}))
        input_el.style = {
            'display': 'inline',
            'width': '240px',
            'margin-left': '20px',
            'padding-left': '12px',
            'padding-right': '18px',
            'border-left-width': '1px',
            'border-right-width': '1px',
        }

        toggle = _append(form, Element('a'))
        toggle.style = {
            'display': 'inline-block',
            'width': '38px',
            'padding-left': '16px',
            'padding-right': '10px',
        }
        _append(toggle, Text('网页'))

        width = _measure_inline_block_intrinsic_width(form, form.style)
        self.assertGreaterEqual(width, 356.0)

    def test_intrinsic_width_skips_absolute_children(self):
        root = Element('div')
        root.style = {'display': 'inline-block'}
        _append(root, Text('hello'))

        base_width = _measure_inline_block_intrinsic_width(root, root.style)

        badge = _append(root, Element('div'))
        badge.style = {
            'display': 'block',
            'position': 'absolute',
            'width': '120px',
        }

        width_with_badge = _measure_inline_block_intrinsic_width(root, root.style)
        self.assertEqual(width_with_badge, base_width)

    def test_inline_collection_skips_absolute_positioned_children(self):
        root = Element('div')
        root.style = {'display': 'block'}

        inline = _append(root, Element('span'))
        inline.style = {'display': 'inline'}
        _append(inline, Text('hello'))

        abs_link = _append(root, Element('a'))
        abs_link.style = {'display': 'inline', 'position': 'absolute'}
        _append(abs_link, Text('ignored'))

        lines, _ = layout_inline(root, 0.0, 0.0, 200.0)
        items = [item for line in lines for item in line.items]
        texts = [item.text for item in items if item.text]
        self.assertEqual(texts, ['hello'])


if __name__ == '__main__':
    unittest.main()
