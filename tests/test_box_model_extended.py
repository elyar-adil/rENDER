"""Extended BoxModel tests covering edge cases."""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest
from layout.box import BoxModel, EdgeSizes, Rect


class TestEdgeSizes(unittest.TestCase):

    def test_default_zeros(self):
        e = EdgeSizes()
        self.assertEqual(e.top, 0.0)
        self.assertEqual(e.right, 0.0)
        self.assertEqual(e.bottom, 0.0)
        self.assertEqual(e.left, 0.0)

    def test_iteration_order(self):
        e = EdgeSizes(top=1, right=2, bottom=3, left=4)
        self.assertEqual(list(e), [1, 2, 3, 4])

    def test_partial_initialization(self):
        e = EdgeSizes(top=10, bottom=20)
        self.assertEqual(e.top, 10)
        self.assertEqual(e.right, 0.0)
        self.assertEqual(e.bottom, 20)
        self.assertEqual(e.left, 0.0)

    def test_float_values(self):
        e = EdgeSizes(top=1.5, right=2.5, bottom=3.5, left=4.5)
        self.assertAlmostEqual(e.top, 1.5)
        self.assertAlmostEqual(e.right, 2.5)
        self.assertAlmostEqual(e.bottom, 3.5)
        self.assertAlmostEqual(e.left, 4.5)


class TestRect(unittest.TestCase):

    def test_basic_construction(self):
        r = Rect(10, 20, 100, 50)
        self.assertEqual(r.x, 10)
        self.assertEqual(r.y, 20)
        self.assertEqual(r.width, 100)
        self.assertEqual(r.height, 50)

    def test_default_values(self):
        r = Rect()
        self.assertEqual(r.x, 0)
        self.assertEqual(r.y, 0)
        self.assertEqual(r.width, 0)
        self.assertEqual(r.height, 0)

    def test_expanded_by_symmetric(self):
        r = Rect(10, 10, 100, 50)
        edges = EdgeSizes(top=5, right=5, bottom=5, left=5)
        expanded = r.expanded_by(edges)
        self.assertEqual(expanded.x, 5)
        self.assertEqual(expanded.y, 5)
        self.assertEqual(expanded.width, 110)
        self.assertEqual(expanded.height, 60)

    def test_expanded_by_asymmetric(self):
        r = Rect(20, 30, 200, 100)
        edges = EdgeSizes(top=10, right=20, bottom=30, left=40)
        expanded = r.expanded_by(edges)
        self.assertEqual(expanded.x, -20)     # 20 - 40
        self.assertEqual(expanded.y, 20)       # 30 - 10
        self.assertEqual(expanded.width, 260)  # 200 + 40 + 20
        self.assertEqual(expanded.height, 140) # 100 + 10 + 30

    def test_expanded_by_zero_edges(self):
        r = Rect(5, 5, 50, 50)
        expanded = r.expanded_by(EdgeSizes())
        self.assertEqual(expanded.x, 5)
        self.assertEqual(expanded.y, 5)
        self.assertEqual(expanded.width, 50)
        self.assertEqual(expanded.height, 50)

    def test_expanded_by_large_edges(self):
        r = Rect(0, 0, 10, 10)
        edges = EdgeSizes(top=100, right=100, bottom=100, left=100)
        expanded = r.expanded_by(edges)
        self.assertEqual(expanded.x, -100)
        self.assertEqual(expanded.y, -100)
        self.assertEqual(expanded.width, 210)
        self.assertEqual(expanded.height, 210)


class TestBoxModelRects(unittest.TestCase):

    def setUp(self):
        self.box = BoxModel()
        self.box.x = 20.0
        self.box.y = 30.0
        self.box.content_width = 100.0
        self.box.content_height = 50.0
        self.box.padding = EdgeSizes(top=5, right=10, bottom=5, left=10)
        self.box.border = EdgeSizes(top=2, right=2, bottom=2, left=2)
        self.box.margin = EdgeSizes(top=8, right=8, bottom=8, left=8)

    def test_content_rect(self):
        cr = self.box.content_rect
        self.assertEqual(cr.x, 20)
        self.assertEqual(cr.y, 30)
        self.assertEqual(cr.width, 100)
        self.assertEqual(cr.height, 50)

    def test_padding_rect(self):
        pr = self.box.padding_rect
        # x = 20 - 10 = 10, y = 30 - 5 = 25, w = 100+20 = 120, h = 50+10 = 60
        self.assertEqual(pr.x, 10)
        self.assertEqual(pr.y, 25)
        self.assertEqual(pr.width, 120)
        self.assertEqual(pr.height, 60)

    def test_border_rect(self):
        br = self.box.border_rect
        # x = 10 - 2 = 8, y = 25 - 2 = 23, w = 120+4 = 124, h = 60+4 = 64
        self.assertEqual(br.x, 8)
        self.assertEqual(br.y, 23)
        self.assertEqual(br.width, 124)
        self.assertEqual(br.height, 64)

    def test_margin_rect(self):
        mr = self.box.margin_rect
        # x = 8 - 8 = 0, y = 23 - 8 = 15, w = 124+16 = 140, h = 64+16 = 80
        self.assertEqual(mr.x, 0)
        self.assertEqual(mr.y, 15)
        self.assertEqual(mr.width, 140)
        self.assertEqual(mr.height, 80)

    def test_border_box_rect_alias(self):
        self.assertEqual(self.box.border_box_rect.x, self.box.border_rect.x)
        self.assertEqual(self.box.border_box_rect.width, self.box.border_rect.width)

    def test_margin_box_rect_alias(self):
        self.assertEqual(self.box.margin_box_rect.x, self.box.margin_rect.x)
        self.assertEqual(self.box.margin_box_rect.width, self.box.margin_rect.width)


class TestBoxModelLegacy(unittest.TestCase):

    def test_update_legacy_syncs_width(self):
        box = BoxModel()
        box.content_width = 150.0
        box.content_height = 75.0
        box.update_legacy()
        self.assertEqual(box.width, 150.0)
        self.assertEqual(box.height, 75.0)
        self.assertEqual(box.h, 75.0)

    def test_initial_legacy_fields_zero(self):
        box = BoxModel()
        self.assertEqual(box.width, 0.0)
        self.assertEqual(box.height, 0.0)
        self.assertEqual(box.h, 0.0)

    def test_repr(self):
        box = BoxModel()
        box.x = 1.0
        box.y = 2.0
        box.content_width = 100.0
        box.content_height = 50.0
        r = repr(box)
        self.assertIn('BoxModel', r)
        self.assertIn('100.0', r)

    def test_zero_padding_rects_equal_content(self):
        box = BoxModel()
        box.x = 0; box.y = 0
        box.content_width = 100; box.content_height = 50
        cr = box.content_rect
        pr = box.padding_rect
        br = box.border_rect
        mr = box.margin_rect
        for rect in (pr, br, mr):
            self.assertEqual(rect.x, cr.x)
            self.assertEqual(rect.y, cr.y)
            self.assertEqual(rect.width, cr.width)
            self.assertEqual(rect.height, cr.height)


if __name__ == '__main__':
    unittest.main()
