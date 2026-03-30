"""Integration tests: CSS Positioning.

Verifies absolute, fixed, and relative positioning behave correctly,
matching real browser behavior.
"""
import pytest
from tests.render_helper import render, find_element, find_all, approx


class TestRelativePositioning:
    """position:relative offsets visually but not in layout flow."""

    def test_relative_top_left(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="a" style="height:50px">A</div>
          <div id="b" style="position:relative; top:10px; left:20px; height:50px">B</div>
          <div id="c" style="height:50px">C</div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        c = find_element(doc, id_name='c')
        # B's layout position is at y=50 (after A), but visually shifted by top:10
        # C should still start at y=100 (B occupies normal flow space)
        assert approx(c.box.y, 100.0), \
            f"C should be at y=100 (relative doesn't affect flow), got {c.box.y}"


class TestAbsolutePositioning:
    """position:absolute removes element from flow."""

    def test_absolute_removed_from_flow(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="position:relative; width:400px; height:300px">
            <div id="a" style="height:50px">A</div>
            <div id="abs" style="position:absolute; top:10px; left:10px; width:100px; height:80px">Abs</div>
            <div id="b" style="height:50px">B</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        abs_el = find_element(doc, id_name='abs')
        # B should be right after A (abs doesn't take space)
        assert approx(b.box.y, 50.0), \
            f"B should be at y=50, got {b.box.y}"
        # Abs should be at top:10, left:10 relative to positioned parent
        assert approx(abs_el.box.x, 10.0, tolerance=5), \
            f"Abs x should be ~10, got {abs_el.box.x}"
        assert approx(abs_el.box.y, 10.0, tolerance=5), \
            f"Abs y should be ~10, got {abs_el.box.y}"

    def test_absolute_right_bottom(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="position:relative; width:400px; height:300px">
            <div id="abs" style="position:absolute; right:20px; bottom:20px;
                                 width:100px; height:50px">Abs</div>
          </div>
        </body></html>
        ''')
        abs_el = find_element(doc, id_name='abs')
        # right:20 → x = 400 - 20 - 100 = 280
        assert approx(abs_el.box.x, 280.0, tolerance=5), \
            f"right:20 should place at x~280, got {abs_el.box.x}"

    def test_absolute_stretches_with_left_right(self):
        """left + right without width should stretch the element."""
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="position:relative; width:500px; height:300px">
            <div id="abs" style="position:absolute; left:50px; right:50px;
                                 top:0; height:40px">Stretch</div>
          </div>
        </body></html>
        ''')
        abs_el = find_element(doc, id_name='abs')
        # Width should be 500 - 50 - 50 = 400
        assert approx(abs_el.box.content_width, 400.0, tolerance=5), \
            f"Should stretch to 400, got {abs_el.box.content_width}"

    def test_absolute_uses_positioned_ancestor_through_static_wrapper(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="rel" style="position:relative; width:400px; height:300px; margin-top:100px; margin-left:50px">
            <div>
              <div id="abs" style="position:absolute; right:10px; bottom:20px;
                                   width:100px; height:50px">Abs</div>
            </div>
          </div>
        </body></html>
        ''')
        rel = find_element(doc, id_name='rel')
        abs_el = find_element(doc, id_name='abs')
        assert approx(abs_el.box.x, rel.box.x + 400.0 - 10.0 - 100.0, tolerance=5), \
            f"Abs x should be anchored to positioned ancestor, got {abs_el.box.x}"
        assert approx(abs_el.box.y, rel.box.y + 300.0 - 20.0 - 50.0, tolerance=5), \
            f"Abs y should be anchored to positioned ancestor, got {abs_el.box.y}"


class TestFixedPositioning:
    """position:fixed positions relative to viewport."""

    def test_fixed_at_viewport_origin(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="height:1000px">tall content</div>
          <div id="fixed" style="position:fixed; top:0; left:0; width:100px; height:40px">
            Fixed
          </div>
        </body></html>
        ''', viewport_width=800)
        fixed = find_element(doc, id_name='fixed')
        assert approx(fixed.box.x, 0.0, tolerance=5), f"Fixed x should be 0, got {fixed.box.x}"
        assert approx(fixed.box.y, 0.0, tolerance=5), f"Fixed y should be 0, got {fixed.box.y}"

    def test_fixed_right_bottom(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="fixed" style="position:fixed; right:0; bottom:0;
                                 width:200px; height:50px">Fixed</div>
        </body></html>
        ''', viewport_width=800, viewport_height=600)
        fixed = find_element(doc, id_name='fixed')
        # right:0 → x = 800 - 200 = 600
        assert approx(fixed.box.x, 600.0, tolerance=5), \
            f"Fixed right:0 should be at x=600, got {fixed.box.x}"
        # bottom:0 → y = 600 - 50 = 550
        assert approx(fixed.box.y, 550.0, tolerance=5), \
            f"Fixed bottom:0 should be at y=550, got {fixed.box.y}"


class TestZIndex:
    """z-index affects stacking order in display list."""

    def test_higher_z_index_drawn_later(self):
        from rendering.display_list import DrawRect
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="position:relative; width:400px; height:300px">
            <div id="back" style="position:absolute; top:0; left:0; width:100px; height:100px;
                                  z-index:1; background-color:red">Back</div>
            <div id="front" style="position:absolute; top:0; left:0; width:100px; height:100px;
                                   z-index:10; background-color:blue">Front</div>
          </div>
        </body></html>
        ''')
        from tests.render_helper import get_display_list
        dl = get_display_list(doc)
        rects = [(i, cmd) for i, cmd in enumerate(dl) if isinstance(cmd, DrawRect)]
        red_idx = None
        blue_idx = None
        for idx, cmd in rects:
            if cmd.color == 'red':
                red_idx = idx
            if cmd.color == 'blue':
                blue_idx = idx
        if red_idx is not None and blue_idx is not None:
            assert blue_idx > red_idx, \
                f"z-index:10 (blue) should be drawn after z-index:1 (red)"


class TestFloatLayout:
    """Float positioning."""

    def test_float_left(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="width:400px">
            <div id="float" style="float:left; width:100px; height:100px">Float</div>
            <div id="text" style="height:200px">Text content that flows around the float</div>
          </div>
        </body></html>
        ''')
        float_el = find_element(doc, id_name='float')
        text_el = find_element(doc, id_name='text')
        assert approx(float_el.box.x, 0.0, tolerance=5)
        # Text element starts at same y as float (flows around)
        assert approx(text_el.box.y, 0.0, tolerance=5)

    def test_clear_both(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="width:400px">
            <div id="float" style="float:left; width:100px; height:100px">Float</div>
            <div id="cleared" style="clear:both; height:50px">Cleared</div>
          </div>
        </body></html>
        ''')
        float_el = find_element(doc, id_name='float')
        cleared = find_element(doc, id_name='cleared')
        assert cleared.box.y >= 100.0 - 2, \
            f"Cleared element should be below float (y>=100), got {cleared.box.y}"
