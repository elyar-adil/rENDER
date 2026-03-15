"""Comprehensive tests for BlockLayout."""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest
from html.dom import Element, Text as TextNode
from layout.box import BoxModel, EdgeSizes
from layout.block import BlockLayout
from layout.context import LayoutContext


def make_element(tag='div', style=None, children=None):
    """Helper: create an Element with given style dict and children."""
    el = Element(tag)
    el.style = style or {}
    for child in (children or []):
        child.parent = el
        el.children.append(child)
    return el


def make_container(x=0, y=0, width=500, height=0):
    """Helper: create a container BoxModel."""
    c = BoxModel()
    c.x = x
    c.y = y
    c.content_width = width
    c.content_height = height
    return c


def do_layout(node, container=None, ctx=None):
    """Run BlockLayout on node inside container."""
    if container is None:
        container = make_container()
    if ctx is None:
        ctx = LayoutContext()
    return BlockLayout().layout(node, container, ctx)


class TestBlockWidth(unittest.TestCase):

    def test_auto_width_fills_container(self):
        el = make_element(style={'display': 'block'})
        box = do_layout(el, make_container(width=400))
        self.assertEqual(box.content_width, 400)

    def test_explicit_px_width(self):
        el = make_element(style={'display': 'block', 'width': '200px'})
        box = do_layout(el, make_container(width=400))
        self.assertEqual(box.content_width, 200)

    def test_percentage_width(self):
        el = make_element(style={'display': 'block', 'width': '50%'})
        box = do_layout(el, make_container(width=400))
        self.assertEqual(box.content_width, 200)

    def test_width_with_padding_content_box(self):
        # content-box: width=200px + padding=20px each side → box x offset is padded
        el = make_element(style={
            'display': 'block', 'width': '200px',
            'padding-left': '20px', 'padding-right': '20px',
            'padding-top': '0px', 'padding-bottom': '0px',
        })
        box = do_layout(el, make_container(width=400))
        self.assertEqual(box.content_width, 200)
        self.assertEqual(box.padding.left, 20)

    def test_width_with_border_box_sizing(self):
        # border-box: width includes padding/border
        el = make_element(style={
            'display': 'block', 'width': '200px', 'box-sizing': 'border-box',
            'padding-left': '20px', 'padding-right': '20px',
            'padding-top': '0px', 'padding-bottom': '0px',
            'border-width-left': '5px', 'border-width-right': '5px',
            'border-width-top': '0px', 'border-width-bottom': '0px',
        })
        box = do_layout(el, make_container(width=400))
        # content_width = 200 - 20 - 20 - 5 - 5 = 150
        self.assertEqual(box.content_width, 150)

    def test_auto_width_subtracts_margin(self):
        el = make_element(style={
            'display': 'block',
            'margin-left': '30px', 'margin-right': '20px',
            'margin-top': '0px', 'margin-bottom': '0px',
        })
        box = do_layout(el, make_container(width=400))
        self.assertEqual(box.content_width, 350)

    def test_min_width_enforced(self):
        el = make_element(style={
            'display': 'block', 'width': '100px', 'min-width': '200px',
        })
        box = do_layout(el, make_container(width=500))
        self.assertEqual(box.content_width, 200)

    def test_max_width_enforced(self):
        el = make_element(style={
            'display': 'block', 'width': '400px', 'max-width': '300px',
        })
        box = do_layout(el, make_container(width=500))
        self.assertEqual(box.content_width, 300)

    def test_min_width_percentage(self):
        el = make_element(style={
            'display': 'block', 'width': '10%', 'min-width': '50%',
        })
        box = do_layout(el, make_container(width=400))
        # 50% of 400 = 200
        self.assertEqual(box.content_width, 200)

    def test_width_zero_not_negative(self):
        # Very wide margin shouldn't result in negative width
        el = make_element(style={
            'display': 'block',
            'margin-left': '300px', 'margin-right': '300px',
            'margin-top': '0px', 'margin-bottom': '0px',
        })
        box = do_layout(el, make_container(width=400))
        self.assertGreaterEqual(box.content_width, 0)


class TestBlockMarginAuto(unittest.TestCase):

    def test_margin_auto_both_centers(self):
        el = make_element(style={
            'display': 'block', 'width': '200px',
            'margin-left': 'auto', 'margin-right': 'auto',
            'margin-top': '0px', 'margin-bottom': '0px',
        })
        box = do_layout(el, make_container(x=0, width=500))
        # Centered: x = (500 - 200) / 2 = 150
        self.assertEqual(box.x, 150)

    def test_margin_left_auto_pushes_right(self):
        el = make_element(style={
            'display': 'block', 'width': '200px',
            'margin-left': 'auto', 'margin-right': '0px',
            'margin-top': '0px', 'margin-bottom': '0px',
        })
        box = do_layout(el, make_container(x=0, width=500))
        # margin-left absorbs all remaining: x = 500 - 200 = 300
        self.assertEqual(box.x, 300)

    def test_margin_right_auto_default_left_align(self):
        el = make_element(style={
            'display': 'block', 'width': '200px',
            'margin-left': '0px', 'margin-right': 'auto',
            'margin-top': '0px', 'margin-bottom': '0px',
        })
        box = do_layout(el, make_container(x=0, width=500))
        # margin-right absorbs: x = 0 (left-aligned)
        self.assertEqual(box.x, 0)

    def test_margin_auto_with_nonzero_container_x(self):
        el = make_element(style={
            'display': 'block', 'width': '100px',
            'margin-left': 'auto', 'margin-right': 'auto',
            'margin-top': '0px', 'margin-bottom': '0px',
        })
        box = do_layout(el, make_container(x=50, width=300))
        # Available: 300 - 100 = 200, split 100 each side → x = 50 + 100 = 150
        self.assertEqual(box.x, 150)


class TestBlockPosition(unittest.TestCase):

    def test_x_offset_includes_margin_and_border(self):
        el = make_element(style={
            'display': 'block',
            'margin-left': '10px', 'margin-top': '0px',
            'margin-right': '0px', 'margin-bottom': '0px',
            'border-width-left': '5px', 'border-width-top': '0px',
            'border-width-right': '0px', 'border-width-bottom': '0px',
            'padding-left': '8px', 'padding-top': '0px',
            'padding-right': '0px', 'padding-bottom': '0px',
        })
        box = do_layout(el, make_container(x=0, y=0, width=500))
        self.assertEqual(box.x, 23)  # 10 + 5 + 8

    def test_y_starts_below_container_content(self):
        # Container has content_height=50, no margin on child
        el = make_element(style={
            'display': 'block',
            'margin-top': '0px', 'margin-left': '0px',
            'margin-right': '0px', 'margin-bottom': '0px',
        })
        container = make_container(x=0, y=100, width=500, height=50)
        box = do_layout(el, container)
        self.assertEqual(box.y, 150)

    def test_margin_top_shifts_y(self):
        el = make_element(style={
            'display': 'block',
            'margin-top': '20px', 'margin-left': '0px',
            'margin-right': '0px', 'margin-bottom': '0px',
        })
        container = make_container(x=0, y=0, width=500, height=0)
        box = do_layout(el, container)
        self.assertEqual(box.y, 20)


class TestBlockHeight(unittest.TestCase):

    def test_auto_height_no_children(self):
        el = make_element(style={'display': 'block'})
        box = do_layout(el)
        self.assertEqual(box.content_height, 0)

    def test_explicit_height(self):
        el = make_element(style={'display': 'block', 'height': '150px'})
        box = do_layout(el)
        self.assertEqual(box.content_height, 150)

    def test_min_height_enforced_on_empty(self):
        el = make_element(style={'display': 'block', 'min-height': '100px'})
        box = do_layout(el)
        self.assertEqual(box.content_height, 100)

    def test_max_height_enforced(self):
        # max-height should cap content_height (overflow:hidden)
        el = make_element(style={
            'display': 'block',
            'height': '300px',
            'max-height': '200px',
            'overflow': 'hidden',
        })
        box = do_layout(el)
        self.assertEqual(box.content_height, 200)

    def test_height_from_block_children(self):
        child1 = make_element(style={
            'display': 'block', 'height': '50px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        child2 = make_element(style={
            'display': 'block', 'height': '80px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[child1, child2])
        box = do_layout(parent)
        self.assertEqual(box.content_height, 130)

    def test_explicit_height_does_not_clip_overflow_visible(self):
        # overflow:visible — height expands if children exceed specified height
        child = make_element(style={
            'display': 'block', 'height': '300px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={
            'display': 'block', 'height': '100px', 'overflow': 'visible',
        }, children=[child])
        box = do_layout(parent)
        self.assertGreaterEqual(box.content_height, 100)


class TestBlockMarginCollapsing(unittest.TestCase):

    def test_adjacent_margins_collapse(self):
        # child1 bottom-margin=20, child2 top-margin=30 → collapsed=30
        child1 = make_element(style={
            'display': 'block', 'height': '50px',
            'margin-top': '0px', 'margin-bottom': '20px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        child2 = make_element(style={
            'display': 'block', 'height': '50px',
            'margin-top': '30px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[child1, child2])
        box = do_layout(parent)
        # child1 height=50, collapsed=30 (max(20,30)), child2 height=50
        # total = 50 + 30 + 50 = 130
        self.assertEqual(box.content_height, 130)

    def test_larger_margin_wins(self):
        child1 = make_element(style={
            'display': 'block', 'height': '10px',
            'margin-top': '0px', 'margin-bottom': '5px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        child2 = make_element(style={
            'display': 'block', 'height': '10px',
            'margin-top': '15px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[child1, child2])
        box = do_layout(parent)
        # collapsed = max(5, 15) = 15, total = 10 + 15 + 10 = 35
        self.assertEqual(box.content_height, 35)

    def test_equal_margins_collapse_to_one(self):
        child1 = make_element(style={
            'display': 'block', 'height': '20px',
            'margin-top': '0px', 'margin-bottom': '10px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        child2 = make_element(style={
            'display': 'block', 'height': '20px',
            'margin-top': '10px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[child1, child2])
        box = do_layout(parent)
        # collapsed = 10, total = 20 + 10 + 20 = 50
        self.assertEqual(box.content_height, 50)


class TestBlockDisplayNone(unittest.TestCase):

    def test_display_none_child_skipped(self):
        visible = make_element(style={
            'display': 'block', 'height': '50px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        hidden = make_element(style={'display': 'none', 'height': '100px'})
        parent = make_element(style={'display': 'block'}, children=[visible, hidden])
        box = do_layout(parent)
        self.assertEqual(box.content_height, 50)

    def test_display_none_has_no_box(self):
        hidden = make_element(style={'display': 'none', 'height': '100px'})
        parent = make_element(style={'display': 'block'}, children=[hidden])
        do_layout(parent)
        self.assertIsNone(hidden.box)


class TestBlockFloats(unittest.TestCase):

    def test_float_left_child(self):
        floated = make_element(style={
            'display': 'block', 'float': 'left',
            'width': '100px', 'height': '80px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[floated])
        ctx = LayoutContext()
        box = do_layout(parent, make_container(width=400), ctx)
        self.assertIsNotNone(floated.box)
        # Float should be positioned at left of parent
        self.assertEqual(floated.box.x, 0)

    def test_float_right_child(self):
        floated = make_element(style={
            'display': 'block', 'float': 'right',
            'width': '100px', 'height': '80px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[floated])
        ctx = LayoutContext()
        box = do_layout(parent, make_container(width=400), ctx)
        self.assertIsNotNone(floated.box)
        # Float should be positioned at right of container
        self.assertEqual(floated.box.x, 300)  # 400 - 100

    def test_clear_left_moves_past_float(self):
        floated = make_element(style={
            'display': 'block', 'float': 'left',
            'width': '100px', 'height': '200px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        cleared = make_element(style={
            'display': 'block', 'clear': 'left', 'height': '50px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[floated, cleared])
        ctx = LayoutContext()
        box = do_layout(parent, make_container(x=0, y=0, width=400), ctx)
        # cleared element should be placed after the float ends (y >= 200)
        self.assertGreaterEqual(cleared.box.y, 200)

    def test_clear_right_moves_past_right_float(self):
        floated = make_element(style={
            'display': 'block', 'float': 'right',
            'width': '100px', 'height': '150px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        cleared = make_element(style={
            'display': 'block', 'clear': 'right', 'height': '30px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[floated, cleared])
        ctx = LayoutContext()
        do_layout(parent, make_container(x=0, y=0, width=400), ctx)
        self.assertGreaterEqual(cleared.box.y, 150)

    def test_clear_both_moves_past_both_floats(self):
        left_float = make_element(style={
            'display': 'block', 'float': 'left',
            'width': '80px', 'height': '100px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        right_float = make_element(style={
            'display': 'block', 'float': 'right',
            'width': '80px', 'height': '200px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        cleared = make_element(style={
            'display': 'block', 'clear': 'both', 'height': '30px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[left_float, right_float, cleared])
        ctx = LayoutContext()
        do_layout(parent, make_container(x=0, y=0, width=400), ctx)
        # Must clear both floats — the taller one is 200px
        self.assertGreaterEqual(cleared.box.y, 200)

    def test_two_left_floats_stack_horizontally(self):
        f1 = make_element(style={
            'display': 'block', 'float': 'left',
            'width': '100px', 'height': '80px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        f2 = make_element(style={
            'display': 'block', 'float': 'left',
            'width': '100px', 'height': '80px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[f1, f2])
        ctx = LayoutContext()
        do_layout(parent, make_container(x=0, y=0, width=400), ctx)
        # Second float should be to the right of first
        self.assertGreater(f2.box.x, f1.box.x)


class TestBlockAbsolutePositioning(unittest.TestCase):

    def test_absolute_with_top_left(self):
        child = make_element(style={
            'display': 'block', 'position': 'absolute',
            'top': '20px', 'left': '30px',
            'width': '100px', 'height': '50px',
            'margin-top': '0px', 'margin-left': '0px',
            'margin-right': '0px', 'margin-bottom': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[child])
        box = do_layout(parent, make_container(x=0, y=0, width=400))
        self.assertIsNotNone(child.box)
        self.assertEqual(child.box.x, 30)
        self.assertEqual(child.box.y, 20)
        self.assertEqual(child.box.content_width, 100)
        self.assertEqual(child.box.content_height, 50)

    def test_absolute_with_right_bottom(self):
        child = make_element(style={
            'display': 'block', 'position': 'absolute',
            'right': '10px', 'bottom': '10px',
            'width': '80px', 'height': '40px',
            'margin-top': '0px', 'margin-left': '0px',
            'margin-right': '0px', 'margin-bottom': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[child])
        ctx = LayoutContext(viewport_width=980, viewport_height=600)
        box = do_layout(parent, make_container(x=0, y=0, width=400), ctx)
        self.assertIsNotNone(child.box)
        # right=10 means x = container_width - 10 - total_w
        # total_w = 80, so x = 400 - 10 - 80 = 310
        self.assertEqual(child.box.content_width, 80)

    def test_absolute_does_not_contribute_to_parent_height(self):
        abs_child = make_element(style={
            'display': 'block', 'position': 'absolute',
            'top': '0px', 'left': '0px',
            'height': '500px', 'width': '100px',
            'margin-top': '0px', 'margin-left': '0px',
            'margin-right': '0px', 'margin-bottom': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[abs_child])
        box = do_layout(parent)
        # Absolute children don't affect parent height
        self.assertEqual(box.content_height, 0)


class TestBlockNestedChildren(unittest.TestCase):

    def test_three_children_stacked(self):
        children = [
            make_element(style={
                'display': 'block', 'height': '30px',
                'margin-top': '0px', 'margin-bottom': '0px',
                'margin-left': '0px', 'margin-right': '0px',
            }) for _ in range(3)
        ]
        parent = make_element(style={'display': 'block'}, children=children)
        box = do_layout(parent)
        self.assertEqual(box.content_height, 90)

    def test_children_positioned_sequentially(self):
        c1 = make_element(style={
            'display': 'block', 'height': '40px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        c2 = make_element(style={
            'display': 'block', 'height': '60px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        parent = make_element(style={'display': 'block'}, children=[c1, c2])
        container = make_container(x=0, y=100, width=400)
        do_layout(parent, container)
        self.assertEqual(c1.box.y, 100)
        self.assertEqual(c2.box.y, 140)

    def test_nested_width_inherits_from_parent(self):
        inner = make_element(style={
            'display': 'block',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        outer = make_element(style={
            'display': 'block', 'width': '300px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        }, children=[inner])
        do_layout(outer, make_container(width=500))
        self.assertEqual(inner.box.content_width, 300)


class TestBlockEdgeSizes(unittest.TestCase):

    def test_padding_all_sides(self):
        el = make_element(style={
            'display': 'block',
            'padding-top': '10px', 'padding-right': '20px',
            'padding-bottom': '30px', 'padding-left': '40px',
        })
        box = do_layout(el)
        self.assertEqual(box.padding.top, 10)
        self.assertEqual(box.padding.right, 20)
        self.assertEqual(box.padding.bottom, 30)
        self.assertEqual(box.padding.left, 40)

    def test_border_all_sides(self):
        el = make_element(style={
            'display': 'block',
            'border-width-top': '1px', 'border-width-right': '2px',
            'border-width-bottom': '3px', 'border-width-left': '4px',
        })
        box = do_layout(el)
        self.assertEqual(box.border.top, 1)
        self.assertEqual(box.border.right, 2)
        self.assertEqual(box.border.bottom, 3)
        self.assertEqual(box.border.left, 4)

    def test_margin_all_sides(self):
        el = make_element(style={
            'display': 'block',
            'margin-top': '5px', 'margin-right': '10px',
            'margin-bottom': '15px', 'margin-left': '20px',
        })
        box = do_layout(el)
        self.assertEqual(box.margin.top, 5)
        self.assertEqual(box.margin.right, 10)
        self.assertEqual(box.margin.bottom, 15)
        self.assertEqual(box.margin.left, 20)

    def test_percentage_padding_uses_container_width(self):
        el = make_element(style={
            'display': 'block',
            'padding-left': '10%', 'padding-right': '0px',
            'padding-top': '0px', 'padding-bottom': '0px',
        })
        box = do_layout(el, make_container(width=200))
        self.assertEqual(box.padding.left, 20)


if __name__ == '__main__':
    unittest.main()
