"""Comprehensive tests for FlexLayout."""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest
from html.dom import Element
from layout.box import BoxModel
from layout.flex import FlexLayout, _compute_x_positions
from layout.context import LayoutContext


def make_element(tag='div', style=None, children=None):
    el = Element(tag)
    el.style = style or {}
    for child in (children or []):
        child.parent = el
        el.children.append(child)
    return el


def make_container(x=0, y=0, width=600, height=0):
    c = BoxModel()
    c.x = x
    c.y = y
    c.content_width = width
    c.content_height = height
    return c


def make_child(width='100px', height='50px', grow='0', shrink='1', extra=None):
    """Helper to create a flex child with common properties."""
    style = {
        'display': 'block',
        'width': width, 'height': height,
        'flex-grow': grow, 'flex-shrink': shrink,
        'margin-top': '0px', 'margin-bottom': '0px',
        'margin-left': '0px', 'margin-right': '0px',
    }
    if extra:
        style.update(extra)
    return make_element(style=style)


def do_flex(parent, container=None, ctx=None):
    if container is None:
        container = make_container()
    if ctx is None:
        ctx = LayoutContext()
    return FlexLayout().layout(parent, container, ctx)


class TestFlexBasic(unittest.TestCase):

    def test_no_children_zero_height(self):
        parent = make_element(style={'display': 'flex'})
        box = do_flex(parent)
        self.assertEqual(box.content_height, 0)

    def test_single_child_positioned(self):
        child = make_child(width='100px', height='50px')
        parent = make_element(style={'display': 'flex'}, children=[child])
        do_flex(parent, make_container(x=0, y=0, width=400))
        self.assertEqual(child.box.x, 0)
        self.assertEqual(child.box.y, 0)

    def test_row_children_side_by_side(self):
        c1 = make_child(width='100px', height='50px')
        c2 = make_child(width='100px', height='50px')
        parent = make_element(style={'display': 'flex', 'flex-direction': 'row'}, children=[c1, c2])
        do_flex(parent, make_container(x=0, y=0, width=400))
        self.assertLess(c1.box.x, c2.box.x)

    def test_row_children_x_positions_no_gap(self):
        c1 = make_child(width='100px', height='50px')
        c2 = make_child(width='100px', height='50px')
        parent = make_element(style={'display': 'flex', 'flex-direction': 'row'}, children=[c1, c2])
        do_flex(parent, make_container(x=0, y=0, width=400))
        self.assertEqual(c1.box.x, 0)
        self.assertEqual(c2.box.x, 100)

    def test_flex_height_from_tallest_child(self):
        c1 = make_child(width='100px', height='30px')
        c2 = make_child(width='100px', height='80px')
        parent = make_element(style={'display': 'flex'}, children=[c1, c2])
        box = do_flex(parent, make_container(width=400))
        self.assertEqual(box.content_height, 80)

    def test_display_none_child_excluded(self):
        c1 = make_child(width='100px', height='50px')
        hidden = make_element(style={'display': 'none', 'width': '200px', 'height': '200px'})
        parent = make_element(style={'display': 'flex'}, children=[c1, hidden])
        box = do_flex(parent, make_container(width=400))
        self.assertEqual(box.content_height, 50)


class TestFlexGrow(unittest.TestCase):

    def test_flex_grow_fills_remaining_space(self):
        child = make_child(width='100px', height='50px', grow='1')
        parent = make_element(style={'display': 'flex'}, children=[child])
        do_flex(parent, make_container(width=400))
        # One child with grow=1 gets all 400px
        self.assertEqual(child.box.content_width, 400)

    def test_flex_grow_proportional_distribution(self):
        c1 = make_child(width='0px', height='50px', grow='1')
        c2 = make_child(width='0px', height='50px', grow='2')
        parent = make_element(style={'display': 'flex'}, children=[c1, c2])
        do_flex(parent, make_container(width=300))
        # c1 gets 100, c2 gets 200
        self.assertAlmostEqual(c1.box.content_width, 100, delta=1)
        self.assertAlmostEqual(c2.box.content_width, 200, delta=1)

    def test_flex_grow_zero_not_expanded(self):
        c1 = make_child(width='100px', height='50px', grow='0')
        c2 = make_child(width='100px', height='50px', grow='1')
        parent = make_element(style={'display': 'flex'}, children=[c1, c2])
        do_flex(parent, make_container(width=400))
        # c1 stays at 100px, c2 gets remaining 300px
        self.assertAlmostEqual(c1.box.content_width, 100, delta=1)
        self.assertAlmostEqual(c2.box.content_width, 300, delta=1)


class TestFlexShrink(unittest.TestCase):

    def test_flex_shrink_reduces_overflow(self):
        c1 = make_child(width='200px', height='50px', grow='0', shrink='1')
        c2 = make_child(width='200px', height='50px', grow='0', shrink='1')
        parent = make_element(style={'display': 'flex'}, children=[c1, c2])
        do_flex(parent, make_container(width=300))
        # Total 400px, container 300px, excess=100 split equally → each 150px
        self.assertAlmostEqual(c1.box.content_width, 150, delta=2)
        self.assertAlmostEqual(c2.box.content_width, 150, delta=2)

    def test_flex_shrink_zero_no_reduction(self):
        c1 = make_child(width='200px', height='50px', grow='0', shrink='0')
        c2 = make_child(width='200px', height='50px', grow='0', shrink='1')
        parent = make_element(style={'display': 'flex'}, children=[c1, c2])
        do_flex(parent, make_container(width=300))
        # c1 has shrink=0, keeps 200px; c2 shrinks
        self.assertAlmostEqual(c1.box.content_width, 200, delta=1)
        self.assertLess(c2.box.content_width, 200)


class TestFlexJustifyContent(unittest.TestCase):

    def _run(self, justify, container_w=500, item_w=100, n=3):
        children = [make_child(width=f'{item_w}px', height='50px') for _ in range(n)]
        parent = make_element(style={'display': 'flex', 'justify-content': justify}, children=children)
        do_flex(parent, make_container(x=0, y=0, width=container_w))
        return [c.box.x for c in children]

    def test_flex_start(self):
        xs = self._run('flex-start')
        self.assertEqual(xs[0], 0)
        self.assertEqual(xs[1], 100)
        self.assertEqual(xs[2], 200)

    def test_flex_end(self):
        xs = self._run('flex-end')
        # Total items = 300, slack = 200; first item at x=200
        self.assertEqual(xs[0], 200)
        self.assertEqual(xs[2], 400)

    def test_center(self):
        xs = self._run('center')
        # slack=200, offset=100; first item at x=100
        self.assertEqual(xs[0], 100)
        self.assertEqual(xs[2], 300)

    def test_space_between(self):
        xs = self._run('space-between', container_w=500, item_w=100, n=3)
        # Gaps: (500-300)/(3-1) = 100 each
        self.assertEqual(xs[0], 0)
        self.assertAlmostEqual(xs[1], 200, delta=1)
        self.assertAlmostEqual(xs[2], 400, delta=1)

    def test_space_around(self):
        xs = self._run('space-around', container_w=500, item_w=100, n=2)
        # around_gap = 300/2 = 150; first at 75, second at 325
        self.assertAlmostEqual(xs[0], 75, delta=1)
        self.assertAlmostEqual(xs[1], 325, delta=1)

    def test_space_between_single_item(self):
        xs = self._run('space-between', container_w=400, item_w=100, n=1)
        self.assertEqual(xs[0], 0)


class TestFlexAlignItems(unittest.TestCase):

    def test_stretch_makes_all_same_height(self):
        c1 = make_child(width='100px', height='30px')
        c2 = make_child(width='100px', height='80px')
        parent = make_element(style={'display': 'flex', 'align-items': 'stretch'}, children=[c1, c2])
        do_flex(parent, make_container(width=400))
        # Both should have the line height (80px)
        self.assertEqual(c1.box.content_height, 80)
        self.assertEqual(c2.box.content_height, 80)

    def test_flex_start_aligns_to_top(self):
        c1 = make_child(width='100px', height='30px')
        c2 = make_child(width='100px', height='80px')
        parent = make_element(style={'display': 'flex', 'align-items': 'flex-start'}, children=[c1, c2])
        do_flex(parent, make_container(x=0, y=0, width=400))
        # Both start at y=0
        self.assertEqual(c1.box.y, 0)
        self.assertEqual(c2.box.y, 0)

    def test_flex_end_aligns_to_bottom(self):
        c1 = make_child(width='100px', height='30px')
        c2 = make_child(width='100px', height='80px')
        parent = make_element(style={'display': 'flex', 'align-items': 'flex-end'}, children=[c1, c2])
        do_flex(parent, make_container(x=0, y=0, width=400))
        # c1 (30px) should be at y=50 (80-30), c2 at y=0
        self.assertEqual(c1.box.y, 50)
        self.assertEqual(c2.box.y, 0)

    def test_center_aligns_vertically(self):
        c1 = make_child(width='100px', height='20px')
        c2 = make_child(width='100px', height='80px')
        parent = make_element(style={'display': 'flex', 'align-items': 'center'}, children=[c1, c2])
        do_flex(parent, make_container(x=0, y=0, width=400))
        # c1: (80-20)/2 = 30
        self.assertEqual(c1.box.y, 30)
        self.assertEqual(c2.box.y, 0)


class TestFlexDirection(unittest.TestCase):

    def test_column_children_stacked_vertically(self):
        c1 = make_child(width='100px', height='50px')
        c2 = make_child(width='100px', height='80px')
        parent = make_element(style={'display': 'flex', 'flex-direction': 'column'}, children=[c1, c2])
        do_flex(parent, make_container(x=0, y=0, width=400))
        self.assertLess(c1.box.y, c2.box.y)
        self.assertEqual(c1.box.y, 0)
        self.assertEqual(c2.box.y, 50)

    def test_column_total_height(self):
        c1 = make_child(width='100px', height='50px')
        c2 = make_child(width='100px', height='80px')
        parent = make_element(style={'display': 'flex', 'flex-direction': 'column'}, children=[c1, c2])
        box = do_flex(parent, make_container(x=0, y=0, width=400))
        self.assertEqual(box.content_height, 130)

    def test_row_reverse_reverses_order(self):
        c1 = make_child(width='100px', height='50px')
        c2 = make_child(width='100px', height='50px')
        parent = make_element(style={'display': 'flex', 'flex-direction': 'row-reverse'}, children=[c1, c2])
        do_flex(parent, make_container(x=0, y=0, width=400))
        # row-reverse: c1 should be to the right of c2
        self.assertGreater(c1.box.x, c2.box.x)


class TestFlexGap(unittest.TestCase):

    def test_gap_between_items(self):
        c1 = make_child(width='100px', height='50px')
        c2 = make_child(width='100px', height='50px')
        parent = make_element(style={'display': 'flex', 'gap': '20px'}, children=[c1, c2])
        do_flex(parent, make_container(x=0, y=0, width=400))
        # c2 should be at x=120 (100 + 20 gap)
        self.assertEqual(c2.box.x, 120)

    def test_column_gap_explicitly_set(self):
        c1 = make_child(width='100px', height='50px')
        c2 = make_child(width='100px', height='50px')
        parent = make_element(style={'display': 'flex', 'column-gap': '30px'}, children=[c1, c2])
        do_flex(parent, make_container(x=0, y=0, width=400))
        self.assertEqual(c2.box.x, 130)

    def test_row_gap_between_wrapped_lines(self):
        # Use shrink=0 to ensure items don't shrink before wrapping
        c1 = make_child(width='200px', height='50px', shrink='0')
        c2 = make_child(width='200px', height='50px', shrink='0')
        parent = make_element(style={
            'display': 'flex', 'flex-wrap': 'wrap', 'row-gap': '15px'
        }, children=[c1, c2])
        do_flex(parent, make_container(x=0, y=0, width=300))
        # c2 wraps to next line; y = line1_height(50) + row_gap(15) = 65
        self.assertAlmostEqual(c2.box.y, 65, delta=1)


class TestFlexWrap(unittest.TestCase):

    def test_nowrap_does_not_wrap(self):
        # Use shrink=0 so items overflow but stay on one line (nowrap)
        children = [make_child(width='200px', height='50px', shrink='0') for _ in range(3)]
        parent = make_element(style={'display': 'flex', 'flex-wrap': 'nowrap'}, children=children)
        box = do_flex(parent, make_container(width=400))
        # All on one line regardless of overflow
        self.assertEqual(box.content_height, 50)

    def test_wrap_breaks_into_two_lines(self):
        # shrink=0: items stay at declared width, 200+200=400 > 300, each wraps
        c1 = make_child(width='200px', height='50px', shrink='0')
        c2 = make_child(width='200px', height='50px', shrink='0')
        parent = make_element(style={'display': 'flex', 'flex-wrap': 'wrap'}, children=[c1, c2])
        box = do_flex(parent, make_container(width=300))
        # Each item on its own line: 2 lines × 50px = 100px
        self.assertEqual(box.content_height, 100)

    def test_wrap_three_items_three_lines(self):
        # 3 items of 200px in 350px, each wraps since 200+200=400 > 350
        c1 = make_child(width='200px', height='50px', shrink='0')
        c2 = make_child(width='200px', height='50px', shrink='0')
        c3 = make_child(width='200px', height='50px', shrink='0')
        parent = make_element(style={'display': 'flex', 'flex-wrap': 'wrap'}, children=[c1, c2, c3])
        box = do_flex(parent, make_container(width=350))
        # Each item on its own line (200+200 > 350): 3 lines × 50px = 150px
        self.assertEqual(box.content_height, 150)

    def test_wrap_two_items_same_line_when_fits(self):
        c1 = make_child(width='100px', height='50px')
        c2 = make_child(width='100px', height='50px')
        parent = make_element(style={'display': 'flex', 'flex-wrap': 'wrap'}, children=[c1, c2])
        box = do_flex(parent, make_container(width=300))
        # Both fit on one line (100+100 < 300)
        self.assertEqual(box.content_height, 50)
        self.assertLess(c1.box.x, c2.box.x)


class TestComputeXPositions(unittest.TestCase):
    """Unit tests for the _compute_x_positions helper."""

    def _make_boxes(self, widths):
        boxes = []
        for w in widths:
            b = BoxModel()
            b.content_width = w
            b.margin = __import__('layout.box', fromlist=['EdgeSizes']).EdgeSizes()
            boxes.append(b)
        return boxes

    def test_flex_start_positions(self):
        boxes = self._make_boxes([100, 100, 100])
        xs = _compute_x_positions('flex-start', boxes, slack=200, gap=0, base_x=0)
        self.assertEqual(xs, [0, 100, 200])

    def test_flex_end_positions(self):
        boxes = self._make_boxes([100, 100])
        xs = _compute_x_positions('flex-end', boxes, slack=100, gap=0, base_x=0)
        self.assertEqual(xs, [100, 200])

    def test_center_positions(self):
        boxes = self._make_boxes([100, 100])
        xs = _compute_x_positions('center', boxes, slack=100, gap=0, base_x=0)
        self.assertEqual(xs, [50, 150])

    def test_space_between_positions(self):
        boxes = self._make_boxes([100, 100, 100])
        xs = _compute_x_positions('space-between', boxes, slack=200, gap=0, base_x=0)
        self.assertAlmostEqual(xs[0], 0, delta=0.5)
        self.assertAlmostEqual(xs[1], 200, delta=0.5)
        self.assertAlmostEqual(xs[2], 400, delta=0.5)

    def test_space_around_positions(self):
        boxes = self._make_boxes([100, 100])
        xs = _compute_x_positions('space-around', boxes, slack=200, gap=0, base_x=0)
        # around_gap = 200/2 = 100; first at 50, second at 250
        self.assertAlmostEqual(xs[0], 50, delta=0.5)
        self.assertAlmostEqual(xs[1], 250, delta=0.5)


if __name__ == '__main__':
    unittest.main()
