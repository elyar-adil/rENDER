"""Comprehensive tests for FloatManager."""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest
from layout.float_manager import FloatManager, FloatBox


class TestFloatManagerEmpty(unittest.TestCase):
    """FloatManager with no floats should return full container width."""

    def setUp(self):
        self.fm = FloatManager()

    def test_no_floats_returns_full_width(self):
        left_x, width = self.fm.available_rect(0, 100, 0, 500)
        self.assertEqual(left_x, 0)
        self.assertEqual(width, 500)

    def test_no_floats_with_nonzero_container_x(self):
        left_x, width = self.fm.available_rect(0, 50, 20, 300)
        self.assertEqual(left_x, 20)
        self.assertEqual(width, 300)

    def test_clear_y_both_no_floats(self):
        self.assertEqual(self.fm.clear_y('both'), 0.0)

    def test_clear_y_left_no_floats(self):
        self.assertEqual(self.fm.clear_y('left'), 0.0)

    def test_clear_y_right_no_floats(self):
        self.assertEqual(self.fm.clear_y('right'), 0.0)

    def test_active_floats_empty(self):
        self.assertEqual(self.fm.active_floats_at(0), [])


class TestFloatManagerSingleLeft(unittest.TestCase):
    """Single left float."""

    def setUp(self):
        self.fm = FloatManager()
        # Float at x=0, y=0, width=100, height=200 on left side
        self.fm.add_float(x=0, y=0, width=100, height=200, side='left')

    def test_overlapping_line_reduces_left(self):
        left_x, width = self.fm.available_rect(y=0, height=20, container_x=0, container_width=500)
        self.assertEqual(left_x, 100)
        self.assertEqual(width, 400)

    def test_line_below_float_gets_full_width(self):
        left_x, width = self.fm.available_rect(y=200, height=20, container_x=0, container_width=500)
        self.assertEqual(left_x, 0)
        self.assertEqual(width, 500)

    def test_line_at_float_bottom_edge_full_width(self):
        # y=200 is exactly where the float ends — should NOT overlap
        left_x, width = self.fm.available_rect(y=200, height=10, container_x=0, container_width=500)
        self.assertEqual(left_x, 0)
        self.assertEqual(width, 500)

    def test_line_spans_across_float(self):
        # A tall line that starts above the float end and extends past it
        left_x, width = self.fm.available_rect(y=100, height=200, container_x=0, container_width=500)
        self.assertEqual(left_x, 100)
        self.assertEqual(width, 400)

    def test_line_starts_before_float_overlaps(self):
        # Line from y=-10 to y=10, float starts at y=0
        left_x, width = self.fm.available_rect(y=-10, height=20, container_x=0, container_width=500)
        self.assertEqual(left_x, 100)
        self.assertEqual(width, 400)

    def test_clear_y_left(self):
        self.assertEqual(self.fm.clear_y('left'), 200.0)

    def test_clear_y_right_unaffected(self):
        self.assertEqual(self.fm.clear_y('right'), 0.0)

    def test_clear_y_both(self):
        self.assertEqual(self.fm.clear_y('both'), 200.0)


class TestFloatManagerSingleRight(unittest.TestCase):
    """Single right float."""

    def setUp(self):
        self.fm = FloatManager()
        # Float at x=400, y=0, width=100, height=150 on right side
        self.fm.add_float(x=400, y=0, width=100, height=150, side='right')

    def test_overlapping_line_reduces_right(self):
        left_x, width = self.fm.available_rect(y=0, height=20, container_x=0, container_width=500)
        self.assertEqual(left_x, 0)
        self.assertEqual(width, 400)

    def test_line_below_float_gets_full_width(self):
        left_x, width = self.fm.available_rect(y=150, height=20, container_x=0, container_width=500)
        self.assertEqual(left_x, 0)
        self.assertEqual(width, 500)

    def test_clear_y_right(self):
        self.assertEqual(self.fm.clear_y('right'), 150.0)

    def test_clear_y_left_unaffected(self):
        self.assertEqual(self.fm.clear_y('left'), 0.0)


class TestFloatManagerBothSides(unittest.TestCase):
    """Left and right floats simultaneously."""

    def setUp(self):
        self.fm = FloatManager()
        # Left float: x=0, y=0, w=100, h=200
        self.fm.add_float(x=0, y=0, width=100, height=200, side='left')
        # Right float: x=400, y=0, w=100, h=150
        self.fm.add_float(x=400, y=0, width=100, height=150, side='right')

    def test_both_floats_active(self):
        left_x, width = self.fm.available_rect(y=0, height=20, container_x=0, container_width=500)
        self.assertEqual(left_x, 100)
        self.assertEqual(width, 300)

    def test_right_float_expired_left_still_active(self):
        # y=150 — right float ends, left still active
        left_x, width = self.fm.available_rect(y=150, height=20, container_x=0, container_width=500)
        self.assertEqual(left_x, 100)
        self.assertEqual(width, 400)

    def test_both_expired(self):
        left_x, width = self.fm.available_rect(y=200, height=20, container_x=0, container_width=500)
        self.assertEqual(left_x, 0)
        self.assertEqual(width, 500)

    def test_clear_y_both_is_max(self):
        self.assertEqual(self.fm.clear_y('both'), 200.0)

    def test_clear_y_left_only(self):
        self.assertEqual(self.fm.clear_y('left'), 200.0)

    def test_clear_y_right_only(self):
        self.assertEqual(self.fm.clear_y('right'), 150.0)


class TestFloatManagerStackedFloats(unittest.TestCase):
    """Multiple floats on same side stacked vertically."""

    def setUp(self):
        self.fm = FloatManager()
        # Two left floats stacked
        self.fm.add_float(x=0, y=0, width=80, height=100, side='left')
        self.fm.add_float(x=0, y=100, width=120, height=100, side='left')

    def test_first_float_active(self):
        left_x, width = self.fm.available_rect(y=0, height=10, container_x=0, container_width=500)
        self.assertEqual(left_x, 80)
        self.assertEqual(width, 420)

    def test_second_float_active(self):
        left_x, width = self.fm.available_rect(y=100, height=10, container_x=0, container_width=500)
        self.assertEqual(left_x, 120)
        self.assertEqual(width, 380)

    def test_clear_y_covers_both(self):
        self.assertEqual(self.fm.clear_y('left'), 200.0)


class TestFloatManagerActiveFloatsAt(unittest.TestCase):

    def setUp(self):
        self.fm = FloatManager()
        self.fm.add_float(x=0, y=10, width=50, height=80, side='left')   # active y=10..89
        self.fm.add_float(x=450, y=30, width=50, height=60, side='right') # active y=30..89

    def test_y_before_any_float(self):
        self.assertEqual(self.fm.active_floats_at(5), [])

    def test_y_only_left_active(self):
        result = self.fm.active_floats_at(15)
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0].side, 'left')

    def test_y_both_active(self):
        result = self.fm.active_floats_at(50)
        self.assertEqual(len(result), 2)

    def test_y_at_float_end_not_included(self):
        # y=90 — both floats end at y=90 (not inclusive)
        result = self.fm.active_floats_at(90)
        self.assertEqual(result, [])


class TestFloatManagerWidthNeverNegative(unittest.TestCase):
    """Width should never go below 0 even if floats exceed container."""

    def test_floats_wider_than_container(self):
        fm = FloatManager()
        fm.add_float(x=0, y=0, width=300, height=100, side='left')
        fm.add_float(x=200, y=0, width=300, height=100, side='right')
        # Both floats active: left=300, right=200 → right < left
        left_x, width = fm.available_rect(y=0, height=10, container_x=0, container_width=500)
        self.assertGreaterEqual(width, 0.0)


if __name__ == '__main__':
    unittest.main()
