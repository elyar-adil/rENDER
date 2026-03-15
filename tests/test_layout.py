"""Tests for the layout engine."""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest


class TestBoxModel(unittest.TestCase):
    def test_box_model_basic(self):
        from layout.box import BoxModel, EdgeSizes, Rect
        box = BoxModel()
        box.x = 10.0
        box.y = 20.0
        box.content_width = 100.0
        box.content_height = 50.0
        box.padding = EdgeSizes(top=5, right=5, bottom=5, left=5)

        cr = box.content_rect
        self.assertEqual(cr.x, 10.0)
        self.assertEqual(cr.y, 20.0)
        self.assertEqual(cr.width, 100.0)

        pr = box.padding_rect
        self.assertEqual(pr.x, 5.0)
        self.assertEqual(pr.width, 110.0)

    def test_rect_expanded_by(self):
        from layout.box import Rect, EdgeSizes
        r = Rect(10, 10, 100, 50)
        edges = EdgeSizes(top=5, right=5, bottom=5, left=5)
        expanded = r.expanded_by(edges)
        self.assertEqual(expanded.x, 5.0)
        self.assertEqual(expanded.y, 5.0)
        self.assertEqual(expanded.width, 110.0)
        self.assertEqual(expanded.height, 60.0)


class TestDisplayList(unittest.TestCase):
    def test_display_list_basic(self):
        from rendering.display_list import DisplayList, DrawRect, DrawText
        from layout.box import Rect

        dl = DisplayList()
        r = Rect(0, 0, 100, 50)
        dl.add(DrawRect(r, 'red'))
        dl.add(DrawText(10, 20, 'Hello', ('Times', 16), 'black'))

        self.assertEqual(len(dl), 2)
        cmds = list(dl)
        self.assertIsInstance(cmds[0], DrawRect)
        self.assertIsInstance(cmds[1], DrawText)


if __name__ == '__main__':
    unittest.main()
