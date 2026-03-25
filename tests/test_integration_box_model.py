"""Integration tests: Box Model and Block Layout.

These tests verify that the full pipeline (HTML → CSS → Layout) produces
correct box positions and sizes, matching real browser behavior.
"""
import pytest
from tests.render_helper import render, find_element, find_all, box_rect, border_rect, approx


class TestBodyMargin:
    """Body has 8px margin by default (UA stylesheet)."""

    def test_body_margin_offsets_child(self):
        doc = render('<div id="d" style="width:100px; height:50px">x</div>')
        d = find_element(doc, id_name='d')
        x, y, w, h = box_rect(d)
        assert x == 8.0, f"Body 8px margin should offset x, got {x}"
        assert y == 8.0, f"Body 8px margin should offset y, got {y}"

    def test_body_zero_margin(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><div id="d" style="width:100px; height:50px">x</div></body>
        </html>''')
        d = find_element(doc, id_name='d')
        x, y, w, h = box_rect(d)
        assert x == 0.0
        assert y == 0.0


class TestContentBox:
    """Default box-sizing: content-box."""

    def test_width_is_content_width(self):
        doc = render('<div id="d" style="width:200px; padding:10px; border:2px solid black">x</div>')
        d = find_element(doc, id_name='d')
        assert d.box.content_width == 200.0
        # Border box should be 200 + 10*2 + 2*2 = 224
        br = d.box.border_rect
        assert approx(br.width, 224.0), f"border_rect width should be 224, got {br.width}"

    def test_padding_adds_to_border_box(self):
        doc = render('<div id="d" style="width:100px; height:50px; padding:20px">x</div>')
        d = find_element(doc, id_name='d')
        assert d.box.content_width == 100.0
        assert d.box.content_height == 50.0
        br = d.box.border_rect
        assert approx(br.width, 140.0)  # 100 + 20*2
        assert approx(br.height, 90.0)  # 50 + 20*2


class TestBorderBox:
    """box-sizing: border-box subtracts padding/border from width."""

    def test_border_box_subtracts_padding(self):
        doc = render('''
        <div id="d" style="box-sizing:border-box; width:200px; padding:20px; height:100px">x</div>
        ''')
        d = find_element(doc, id_name='d')
        # content_width = 200 - 20*2 = 160
        assert approx(d.box.content_width, 160.0), \
            f"border-box content width should be 160, got {d.box.content_width}"

    def test_border_box_subtracts_border_and_padding(self):
        doc = render('''
        <div id="d" style="box-sizing:border-box; width:200px; padding:10px;
                          border:5px solid black; height:100px">x</div>
        ''')
        d = find_element(doc, id_name='d')
        # content_width = 200 - 10*2 - 5*2 = 170
        assert approx(d.box.content_width, 170.0), \
            f"Expected 170, got {d.box.content_width}"


class TestBlockWidth:
    """Block elements fill their container by default."""

    def test_auto_width_fills_container(self):
        """A block div without width should fill the container minus margins."""
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><div id="d" style="height:50px">x</div></body></html>
        ''', viewport_width=1000)
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 1000.0), \
            f"Auto width div should fill viewport, got {d.box.content_width}"

    def test_percentage_width(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><div id="d" style="width:50%; height:50px">x</div></body></html>
        ''', viewport_width=1000)
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 500.0), \
            f"50% width should be 500, got {d.box.content_width}"

    def test_min_width(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><div id="d" style="width:100px; min-width:200px; height:10px">x</div></body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert d.box.content_width >= 200.0, \
            f"min-width should enforce 200, got {d.box.content_width}"

    def test_max_width(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><div id="d" style="width:500px; max-width:200px; height:10px">x</div></body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert d.box.content_width <= 200.0, \
            f"max-width should cap at 200, got {d.box.content_width}"


class TestMarginAuto:
    """margin:auto should center a block element."""

    def test_margin_auto_centers_horizontally(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><div id="d" style="width:200px; margin:0 auto; height:50px">x</div></body></html>
        ''', viewport_width=800)
        d = find_element(doc, id_name='d')
        # Should be centered: (800 - 200) / 2 = 300
        assert approx(d.box.x, 300.0), \
            f"margin:auto should center at x=300, got {d.box.x}"


class TestMarginCollapsing:
    """Adjacent vertical margins collapse."""

    def test_adjacent_siblings_collapse(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="a" style="margin-bottom:30px; height:50px">A</div>
          <div id="b" style="margin-top:20px; height:50px">B</div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        # Collapsed margin = max(30, 20) = 30
        gap = b.box.y - (a.box.y + a.box.content_height)
        assert approx(gap, 30.0), \
            f"Collapsed margin should be 30, gap is {gap}"

    def test_equal_margins_collapse(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="a" style="margin-bottom:20px; height:50px">A</div>
          <div id="b" style="margin-top:20px; height:50px">B</div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        gap = b.box.y - (a.box.y + a.box.content_height)
        assert approx(gap, 20.0), \
            f"Equal margins should collapse to 20, gap is {gap}"


class TestBlockStacking:
    """Block elements stack vertically."""

    def test_three_blocks_stack(self):
        doc = render('''
        <html><head><style>body { margin: 0; } div { margin: 0; }</style></head>
        <body>
          <div id="a" style="height:100px">A</div>
          <div id="b" style="height:100px">B</div>
          <div id="c" style="height:100px">C</div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        c = find_element(doc, id_name='c')
        assert approx(a.box.y, 0.0)
        assert approx(b.box.y, 100.0), f"B should start at y=100, got {b.box.y}"
        assert approx(c.box.y, 200.0), f"C should start at y=200, got {c.box.y}"

    def test_blocks_with_padding_stack(self):
        doc = render('''
        <html><head><style>body { margin: 0; } div { margin: 0; }</style></head>
        <body>
          <div id="a" style="height:50px; padding:10px">A</div>
          <div id="b" style="height:50px">B</div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        # A border box height = 50 + 10*2 = 70
        assert approx(b.box.y, 70.0), f"B should start at y=70, got {b.box.y}"


class TestDisplayNone:
    """display:none removes element from layout."""

    def test_hidden_element_has_no_box(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="a" style="height:50px">A</div>
          <div id="hidden" style="display:none; height:100px">Hidden</div>
          <div id="b" style="height:50px">B</div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        assert approx(b.box.y, 50.0), \
            f"B should be at y=50 (hidden div takes no space), got {b.box.y}"


class TestNestedBlocks:
    """Nested blocks inherit container width."""

    def test_nested_div_fills_parent(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="outer" style="width:400px; padding:20px">
            <div id="inner" style="height:50px">inner</div>
          </div>
        </body></html>
        ''')
        outer = find_element(doc, id_name='outer')
        inner = find_element(doc, id_name='inner')
        assert approx(outer.box.content_width, 400.0)
        assert approx(inner.box.content_width, 400.0), \
            f"Inner should fill parent content area (400), got {inner.box.content_width}"

    def test_nested_percentage_width(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="outer" style="width:400px">
            <div id="inner" style="width:50%; height:50px">inner</div>
          </div>
        </body></html>
        ''')
        inner = find_element(doc, id_name='inner')
        assert approx(inner.box.content_width, 200.0), \
            f"50% of 400 = 200, got {inner.box.content_width}"


class TestHeightBehavior:
    """Height auto vs explicit."""

    def test_auto_height_wraps_children(self):
        doc = render('''
        <html><head><style>body { margin: 0; } div { margin: 0; }</style></head>
        <body>
          <div id="parent">
            <div style="height:100px">child1</div>
            <div style="height:100px">child2</div>
          </div>
        </body></html>
        ''')
        parent = find_element(doc, id_name='parent')
        assert parent.box.content_height >= 200.0, \
            f"Parent should be at least 200px tall, got {parent.box.content_height}"

    def test_explicit_height(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><div id="d" style="height:300px">x</div></body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_height, 300.0)


class TestOverflowHidden:
    """overflow:hidden clips content but specified height is used."""

    def test_overflow_hidden_uses_specified_height(self):
        doc = render('''
        <html><head><style>body { margin: 0; } div { margin: 0; }</style></head>
        <body>
          <div id="d" style="height:100px; overflow:hidden">
            <div style="height:500px">tall</div>
          </div>
          <div id="after" style="height:50px">after</div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        after = find_element(doc, id_name='after')
        assert approx(d.box.content_height, 100.0), \
            f"overflow:hidden with height should be 100, got {d.box.content_height}"
        assert approx(after.box.y, 100.0), \
            f"Next element should start at y=100, got {after.box.y}"
