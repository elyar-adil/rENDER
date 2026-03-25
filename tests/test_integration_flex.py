"""Integration tests: Flexbox Layout.

Verifies that flex containers distribute space correctly,
matching real browser flexbox behavior.
"""
import pytest
from tests.render_helper import render, find_element, find_all, approx


class TestFlexBasicRow:
    """Default flex-direction: row."""

    def test_flex_children_side_by_side(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; width:600px">
            <div id="a" style="width:200px; height:50px">A</div>
            <div id="b" style="width:200px; height:50px">B</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        # A should be at x=0, B at x=200
        assert approx(a.box.x, 0.0)
        assert approx(b.box.x, 200.0), f"B should start at x=200, got {b.box.x}"
        assert approx(a.box.y, b.box.y), "Same row means same y"

    def test_flex_grow_distributes_space(self):
        """flex-grow distributes space proportionally (1:2 ratio)."""
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; width:600px">
            <div id="a" style="flex-grow:1; height:50px">A</div>
            <div id="b" style="flex-grow:2; height:50px">B</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        # Items have small intrinsic text width; remaining space distributed 1:2
        # Key check: B should be approximately 2x A's width
        ratio = b.box.content_width / a.box.content_width if a.box.content_width > 0 else 0
        assert 1.8 <= ratio <= 2.2, \
            f"flex-grow 1:2 ratio should be ~2, got {ratio:.2f} (A={a.box.content_width:.0f}, B={b.box.content_width:.0f})"
        # Total should fill container
        total = a.box.content_width + b.box.content_width
        assert approx(total, 600.0, tolerance=5), \
            f"Total should fill 600px, got {total}"

    def test_flex_shrink(self):
        """Items shrink when total exceeds container."""
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; width:300px">
            <div id="a" style="width:200px; flex-shrink:1; height:50px">A</div>
            <div id="b" style="width:200px; flex-shrink:1; height:50px">B</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        # Total = 400, container = 300, each shrinks by 50
        assert approx(a.box.content_width, 150.0), \
            f"A should shrink to 150, got {a.box.content_width}"
        assert approx(b.box.content_width, 150.0), \
            f"B should shrink to 150, got {b.box.content_width}"


class TestFlexColumn:
    """flex-direction: column stacks vertically."""

    def test_column_stacks_vertically(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; flex-direction:column; width:300px">
            <div id="a" style="height:100px">A</div>
            <div id="b" style="height:100px">B</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        assert approx(a.box.y, 0.0)
        assert approx(b.box.y, 100.0), f"B should be at y=100, got {b.box.y}"

    def test_column_flex_grow(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; flex-direction:column; width:300px; height:400px">
            <div id="a" style="flex-grow:1">A</div>
            <div id="b" style="flex-grow:1">B</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        assert approx(a.box.content_height, 200.0), \
            f"Each should get 200px height, A got {a.box.content_height}"


class TestJustifyContent:
    """justify-content distributes space along main axis."""

    def test_center(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; justify-content:center; width:600px">
            <div id="a" style="width:100px; height:50px">A</div>
            <div id="b" style="width:100px; height:50px">B</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        # Free space = 600 - 200 = 400, offset = 200
        assert approx(a.box.x, 200.0), f"A should be at x=200, got {a.box.x}"
        assert approx(b.box.x, 300.0), f"B should be at x=300, got {b.box.x}"

    def test_space_between(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; justify-content:space-between; width:600px">
            <div id="a" style="width:100px; height:50px">A</div>
            <div id="b" style="width:100px; height:50px">B</div>
            <div id="c" style="width:100px; height:50px">C</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        c = find_element(doc, id_name='c')
        # Free space = 600 - 300 = 300, gaps = 2, each gap = 150
        assert approx(a.box.x, 0.0)
        assert approx(b.box.x, 250.0), f"B should be at x=250, got {b.box.x}"
        assert approx(c.box.x, 500.0), f"C should be at x=500, got {c.box.x}"

    def test_space_around(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; justify-content:space-around; width:600px">
            <div id="a" style="width:100px; height:50px">A</div>
            <div id="b" style="width:100px; height:50px">B</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        # Free space = 600 - 200 = 400, 2 items → each gets 200 space
        # A center at 100, B center at 500 → A.x=50, B.x=350
        assert approx(a.box.x, 100.0), f"A should be at x=100, got {a.box.x}"
        assert approx(b.box.x, 400.0), f"B should be at x=400, got {b.box.x}"

    def test_flex_end(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; justify-content:flex-end; width:600px">
            <div id="a" style="width:100px; height:50px">A</div>
            <div id="b" style="width:100px; height:50px">B</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        assert approx(a.box.x, 400.0), f"A should be at x=400, got {a.box.x}"
        assert approx(b.box.x, 500.0), f"B should be at x=500, got {b.box.x}"


class TestAlignItems:
    """align-items controls cross-axis alignment."""

    def test_stretch_default(self):
        """Items stretch to fill container height by default."""
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; width:400px; height:200px">
            <div id="a" style="width:100px">A</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        assert approx(a.box.content_height, 200.0), \
            f"Stretch should make height 200, got {a.box.content_height}"

    def test_center(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; align-items:center; width:400px; height:200px">
            <div id="a" style="width:100px; height:50px">A</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        # Centered: (200 - 50) / 2 = 75
        assert approx(a.box.y, 75.0), \
            f"align-items:center should center at y=75, got {a.box.y}"

    def test_flex_end(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; align-items:flex-end; width:400px; height:200px">
            <div id="a" style="width:100px; height:50px">A</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        assert approx(a.box.y, 150.0), \
            f"flex-end should put at y=150, got {a.box.y}"


class TestFlexGap:
    """gap property in flex containers."""

    def test_column_gap(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; gap:20px; width:600px">
            <div id="a" style="width:100px; height:50px">A</div>
            <div id="b" style="width:100px; height:50px">B</div>
            <div id="c" style="width:100px; height:50px">C</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        c = find_element(doc, id_name='c')
        assert approx(a.box.x, 0.0)
        assert approx(b.box.x, 120.0), f"B should be at 100+20=120, got {b.box.x}"
        assert approx(c.box.x, 240.0), f"C should be at 220+20=240, got {c.box.x}"


class TestFlexWrap:
    """flex-wrap wraps items to next line."""

    def test_wrap_overflows_to_next_line(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; flex-wrap:wrap; width:300px">
            <div id="a" style="width:200px; height:50px">A</div>
            <div id="b" style="width:200px; height:50px">B</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        # B should wrap to next line
        assert approx(a.box.y, 0.0)
        assert b.box.y >= 50.0 - 2, \
            f"B should wrap to y>=50, got {b.box.y}"


class TestFlexDisplayNone:
    """display:none flex children are excluded."""

    def test_hidden_child_excluded(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:flex; width:600px">
            <div id="a" style="width:100px; height:50px">A</div>
            <div style="display:none; width:100px; height:50px">Hidden</div>
            <div id="b" style="width:100px; height:50px">B</div>
          </div>
        </body></html>
        ''')
        a = find_element(doc, id_name='a')
        b = find_element(doc, id_name='b')
        assert approx(b.box.x, 100.0), \
            f"B should be right after A at x=100, got {b.box.x}"


class TestFlexRealWorldPatterns:
    """Common real-world flexbox patterns."""

    def test_holy_grail_layout(self):
        """Header + (sidebar + content + sidebar) + footer."""
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          .container { display: flex; flex-direction: column; height: 500px; }
          .middle { display: flex; flex-grow: 1; }
          .sidebar { width: 200px; }
          .content { flex-grow: 1; }
        </style></head>
        <body>
          <div class="container">
            <div id="header" style="height:60px">Header</div>
            <div class="middle">
              <div id="left" class="sidebar">Left</div>
              <div id="content" class="content">Content</div>
              <div id="right" class="sidebar">Right</div>
            </div>
            <div id="footer" style="height:40px">Footer</div>
          </div>
        </body></html>
        ''', viewport_width=1000)
        header = find_element(doc, id_name='header')
        left = find_element(doc, id_name='left')
        content = find_element(doc, id_name='content')
        right = find_element(doc, id_name='right')
        footer = find_element(doc, id_name='footer')

        # Header at top
        assert approx(header.box.y, 0.0)
        assert approx(header.box.content_height, 60.0)

        # Sidebars at 200px each
        assert approx(left.box.content_width, 200.0)
        assert approx(right.box.content_width, 200.0)

        # Content fills remaining: 1000 - 200 - 200 = 600
        assert approx(content.box.content_width, 600.0), \
            f"Content should be 600px wide, got {content.box.content_width}"

        # Left at x=0, content at x=200, right at x=800
        assert approx(left.box.x, 0.0)
        assert approx(content.box.x, 200.0)
        assert approx(right.box.x, 800.0)

    def test_card_grid_with_wrap(self):
        """Cards wrapping in a flex container."""
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          .grid { display: flex; flex-wrap: wrap; width: 400px; }
          .card { width: 180px; height: 100px; margin: 5px; }
        </style></head>
        <body>
          <div class="grid">
            <div class="card" id="c1">1</div>
            <div class="card" id="c2">2</div>
            <div class="card" id="c3">3</div>
            <div class="card" id="c4">4</div>
          </div>
        </body></html>
        ''')
        c1 = find_element(doc, id_name='c1')
        c2 = find_element(doc, id_name='c2')
        c3 = find_element(doc, id_name='c3')
        c4 = find_element(doc, id_name='c4')

        # Each card is 180+5+5=190 wide, 2 fit per row (380 < 400)
        # Row 1: c1, c2. Row 2: c3, c4
        assert approx(c1.box.y, c2.box.y), "c1 and c2 on same row"
        assert c3.box.y > c1.box.y + 50, "c3 should be on next row"
        assert approx(c3.box.y, c4.box.y), "c3 and c4 on same row"

    def test_centered_content(self):
        """Center a single item both horizontally and vertically."""
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          .center { display: flex; justify-content: center; align-items: center;
                    width: 500px; height: 400px; }
        </style></head>
        <body>
          <div class="center">
            <div id="item" style="width:100px; height:80px">centered</div>
          </div>
        </body></html>
        ''')
        item = find_element(doc, id_name='item')
        # x: (500 - 100) / 2 = 200
        # y: (400 - 80) / 2 = 160
        assert approx(item.box.x, 200.0), \
            f"Should be centered at x=200, got {item.box.x}"
        assert approx(item.box.y, 160.0), \
            f"Should be centered at y=160, got {item.box.y}"
