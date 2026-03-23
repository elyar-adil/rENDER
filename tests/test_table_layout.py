"""Tests for table layout behavior."""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest

from html.dom import Element
from layout.box import BoxModel
from layout.block import BlockLayout
from layout.context import LayoutContext
from layout.table import TableLayout, _compute_col_widths


def _append(parent, child):
    child.parent = parent
    parent.children.append(child)
    return child


class TestTableColumnWidths(unittest.TestCase):
    def test_auto_columns_use_intrinsic_content_widths(self):
        row = Element('tr')
        left = Element('td', {'width': '18'})
        right = Element('td')
        right.style = {'font-size': '16px'}
        _append(right, Element('img'))
        right.children[0].natural_width = 200
        _append(row, left)
        _append(row, right)

        widths = _compute_col_widths([(row, [(left, 1), (right, 1)])], 2, 400, 0)

        self.assertAlmostEqual(widths[0], 18.0)
        self.assertGreater(widths[1], widths[0])


class TestTableCellAlignment(unittest.TestCase):
    def test_table_cell_honors_text_and_vertical_alignment(self):
        table = Element('table', {'cellspacing': '0', 'cellpadding': '0', 'width': '200'})
        tr = _append(table, Element('tr'))
        td = _append(tr, Element('td'))
        td.style = {
            'text-align': 'right',
            'vertical-align': 'bottom',
            'padding-top': '0px',
            'padding-right': '0px',
            'padding-bottom': '0px',
            'padding-left': '0px',
        }

        inner = _append(td, Element('div'))
        inner.style = {'display': 'block', 'width': '50px', 'height': '20px'}

        td2 = _append(tr, Element('td'))
        inner2 = _append(td2, Element('div'))
        inner2.style = {'display': 'block', 'width': '50px', 'height': '60px'}

        container = BoxModel()
        container.x = 0.0
        container.y = 0.0
        container.content_width = 200.0
        container.content_height = 0.0

        ctx = LayoutContext(viewport_width=200, viewport_height=200)
        TableLayout().layout(table, container, ctx)

        self.assertAlmostEqual(inner.box.x, 50.0)
        self.assertAlmostEqual(inner.box.y, 40.0)


class TestBlockBrAroundBlockChildren(unittest.TestCase):
    def test_br_between_block_children_is_not_double_counted(self):
        container = Element('div')
        container.style = {
            'display': 'block',
            'font-size': '10px',
        }

        top = _append(container, Element('div'))
        top.style = {
            'display': 'block',
            'height': '20px',
        }

        _append(container, Element('br'))

        bottom = _append(container, Element('div'))
        bottom.style = {
            'display': 'block',
            'height': '30px',
        }

        outer = BoxModel()
        outer.x = 0.0
        outer.y = 0.0
        outer.content_width = 200.0
        outer.content_height = 0.0

        ctx = LayoutContext(viewport_width=200, viewport_height=200)
        box = BlockLayout().layout(container, outer, ctx)

        self.assertAlmostEqual(box.content_height, 62.0)


if __name__ == '__main__':
    unittest.main()
