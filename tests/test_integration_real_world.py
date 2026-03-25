"""Integration tests: Real-World Page Patterns.

Tests common patterns found on real websites to verify the engine
handles them correctly end-to-end.
"""
import pytest
from tests.render_helper import render, find_element, find_all, approx, get_display_list
from rendering.display_list import DrawRect, DrawText, DrawBorder


class TestNavigationBar:
    """Horizontal navigation bar pattern."""

    def test_horizontal_nav_with_flex(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          nav { display: flex; background-color: #333; height: 60px; }
          nav a { color: white; padding: 20px; text-decoration: none; }
        </style></head>
        <body>
          <nav id="nav">
            <a href="#" id="a1">Home</a>
            <a href="#" id="a2">About</a>
            <a href="#" id="a3">Contact</a>
          </nav>
        </body></html>
        ''', viewport_width=1000)
        nav = find_element(doc, id_name='nav')
        a1 = find_element(doc, id_name='a1')
        a2 = find_element(doc, id_name='a2')
        # Nav should fill width
        assert approx(nav.box.content_width, 1000.0, tolerance=5)
        # Links should be side by side
        assert a2.box.x > a1.box.x, "Links should be left to right"
        # Nav has background
        dl = get_display_list(doc)
        bg_rects = [cmd for cmd in dl if isinstance(cmd, DrawRect) and cmd.color == '#333']
        assert len(bg_rects) > 0, "Nav should have background color rendered"


class TestTwoColumnLayout:
    """Classic sidebar + content layout."""

    def test_sidebar_content_layout(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          .wrapper { display: flex; }
          .sidebar { width: 250px; min-height: 100px; }
          .content { flex-grow: 1; min-height: 100px; }
        </style></head>
        <body>
          <div class="wrapper">
            <div class="sidebar" id="side">Sidebar</div>
            <div class="content" id="main">Main content area</div>
          </div>
        </body></html>
        ''', viewport_width=1000)
        side = find_element(doc, id_name='side')
        main = find_element(doc, id_name='main')
        assert approx(side.box.content_width, 250.0)
        assert main.box.content_width > 700, \
            f"Content should fill remaining space, got {main.box.content_width}"
        assert main.box.x > side.box.x, "Content should be right of sidebar"


class TestCardLayout:
    """Card component pattern."""

    def test_card_with_padding_border(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          .card {
            width: 300px;
            padding: 16px;
            border: 1px solid #ddd;
            border-radius: 8px;
            margin: 16px auto;
            background-color: white;
          }
          .card h2 { margin: 0 0 8px 0; font-size: 20px; }
          .card p { margin: 0; }
        </style></head>
        <body>
          <div class="card" id="card">
            <h2>Card Title</h2>
            <p>Card content goes here with some text.</p>
          </div>
        </body></html>
        ''', viewport_width=800)
        card = find_element(doc, id_name='card')
        assert approx(card.box.content_width, 300.0)
        assert card.box.padding.top == 16.0
        assert card.box.border.top == 1.0
        # Card should be centered (margin: auto)
        expected_x = (800 - 300 - 16*2 - 1*2) / 2 + 1 + 16
        assert approx(card.box.x, expected_x, tolerance=5), \
            f"Card should be centered at ~{expected_x}, got {card.box.x}"


class TestHeaderContentFooter:
    """Common page structure: header, content, footer."""

    def test_basic_page_structure(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          header, main, footer { display: block; }
          header { height: 80px; background-color: #333; }
          main { min-height: 400px; }
          footer { height: 60px; background-color: #666; }
        </style></head>
        <body>
          <header id="hdr">Header</header>
          <main id="main">Content</main>
          <footer id="ftr">Footer</footer>
        </body></html>
        ''', viewport_width=1000)
        hdr = find_element(doc, id_name='hdr')
        main = find_element(doc, id_name='main')
        ftr = find_element(doc, id_name='ftr')
        assert approx(hdr.box.y, 0.0)
        assert approx(hdr.box.content_height, 80.0)
        assert approx(main.box.y, 80.0)
        assert main.box.content_height >= 400.0
        assert ftr.box.y >= 480.0 - 2


class TestListRendering:
    """Ordered and unordered lists."""

    def test_list_items_stack_vertically(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <ul id="list" style="margin:0; padding:0 0 0 40px">
            <li id="li1">Item 1</li>
            <li id="li2">Item 2</li>
            <li id="li3">Item 3</li>
          </ul>
        </body></html>
        ''')
        li1 = find_element(doc, id_name='li1')
        li2 = find_element(doc, id_name='li2')
        li3 = find_element(doc, id_name='li3')
        assert li2.box.y > li1.box.y, "li2 below li1"
        assert li3.box.y > li2.box.y, "li3 below li2"

    def test_list_items_indented(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <ul style="margin:0; padding-left:40px">
            <li id="li">Item</li>
          </ul>
        </body></html>
        ''')
        li = find_element(doc, id_name='li')
        assert li.box.x >= 40.0 - 2, \
            f"List item should be indented by padding-left, x={li.box.x}"


class TestTableLayout:
    """Basic table layout."""

    def test_table_cells_side_by_side(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <table id="t" style="width:400px; border-collapse:collapse">
            <tr>
              <td id="c1" style="width:200px; height:50px">Cell 1</td>
              <td id="c2" style="width:200px; height:50px">Cell 2</td>
            </tr>
          </table>
        </body></html>
        ''')
        c1 = find_element(doc, id_name='c1')
        c2 = find_element(doc, id_name='c2')
        assert c2.box.x > c1.box.x, "Cell 2 should be right of Cell 1"
        assert approx(c1.box.y, c2.box.y, tolerance=5), "Same row means same y"

    def test_table_rows_stack(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <table style="width:400px">
            <tr><td id="r1" style="height:50px">Row 1</td></tr>
            <tr><td id="r2" style="height:50px">Row 2</td></tr>
          </table>
        </body></html>
        ''')
        r1 = find_element(doc, id_name='r1')
        r2 = find_element(doc, id_name='r2')
        assert r2.box.y > r1.box.y, "Row 2 should be below Row 1"


class TestFormElements:
    """Form input rendering."""

    def test_text_input_has_size(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <form>
            <input type="text" id="inp" style="width:200px; height:30px">
          </form>
        </body></html>
        ''')
        # Input should produce DrawInput in display list
        dl = get_display_list(doc)
        from rendering.display_list import DrawInput
        inputs = [cmd for cmd in dl if isinstance(cmd, DrawInput)]
        assert len(inputs) > 0, "Should render text input"


class TestHeadingHierarchy:
    """Heading sizes decrease from h1 to h6."""

    def test_heading_sizes(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <h1 id="h1" style="margin:0">H1</h1>
          <h2 id="h2" style="margin:0">H2</h2>
          <h3 id="h3" style="margin:0">H3</h3>
        </body></html>
        ''')
        h1 = find_element(doc, id_name='h1')
        h2 = find_element(doc, id_name='h2')
        h3 = find_element(doc, id_name='h3')
        # Font sizes should decrease
        h1_fs = float(h1.style.get('font-size', '0').replace('px', ''))
        h2_fs = float(h2.style.get('font-size', '0').replace('px', ''))
        h3_fs = float(h3.style.get('font-size', '0').replace('px', ''))
        assert h1_fs > h2_fs > h3_fs, \
            f"h1 > h2 > h3 font-size: {h1_fs}, {h2_fs}, {h3_fs}"
        # All should be bold
        assert h1.style.get('font-weight') in ('bold', '700')


class TestBackgroundAndBorder:
    """Background colors and borders render correctly."""

    def test_background_color_renders(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="d" style="width:200px; height:100px; background-color:red">x</div>
        </body></html>
        ''')
        dl = get_display_list(doc)
        red_rects = [cmd for cmd in dl if isinstance(cmd, DrawRect) and cmd.color == 'red']
        assert len(red_rects) > 0, "Should draw red background"

    def test_border_renders(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="d" style="width:200px; height:100px; border:3px solid blue">x</div>
        </body></html>
        ''')
        dl = get_display_list(doc)
        borders = [cmd for cmd in dl if isinstance(cmd, DrawBorder)]
        assert len(borders) > 0, "Should draw border"


class TestResponsiveLayout:
    """Layout changes based on viewport width."""

    def test_narrow_viewport_adjusts_width(self):
        """Elements with % width adapt to narrow viewport."""
        doc_wide = render('''
        <html><head><style>
          body { margin: 0; }
          #d { width: 80%; height: 10px; }
        </style></head>
        <body><div id="d">x</div></body></html>
        ''', viewport_width=1000)
        doc_narrow = render('''
        <html><head><style>
          body { margin: 0; }
          #d { width: 80%; height: 10px; }
        </style></head>
        <body><div id="d">x</div></body></html>
        ''', viewport_width=400)
        d_wide = find_element(doc_wide, id_name='d')
        d_narrow = find_element(doc_narrow, id_name='d')
        assert approx(d_wide.box.content_width, 800.0)
        assert approx(d_narrow.box.content_width, 320.0)

    def test_media_query_responsive(self):
        """@media query changes layout at different widths."""
        html = '''
        <html><head><style>
          body { margin: 0; }
          .container { display: flex; flex-direction: row; }
          .sidebar { width: 200px; height: 100px; }
          .content { flex-grow: 1; height: 100px; }
          @media (max-width: 600px) {
            .container { flex-direction: column; }
            .sidebar { width: 100%; }
          }
        </style></head>
        <body>
          <div class="container" id="wrap">
            <div class="sidebar" id="side">Sidebar</div>
            <div class="content" id="main">Content</div>
          </div>
        </body></html>
        '''
        # Wide: side by side
        doc_wide = render(html, viewport_width=1000)
        side_wide = find_element(doc_wide, id_name='side')
        main_wide = find_element(doc_wide, id_name='main')
        assert approx(side_wide.box.y, main_wide.box.y, tolerance=5), \
            "Wide: sidebar and content on same row"

        # Narrow: stacked
        doc_narrow = render(html, viewport_width=400)
        side_narrow = find_element(doc_narrow, id_name='side')
        main_narrow = find_element(doc_narrow, id_name='main')
        assert main_narrow.box.y > side_narrow.box.y, \
            "Narrow: content should be below sidebar"


class TestPseudoElements:
    """::before and ::after pseudo-elements."""

    def test_before_pseudo_renders(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          .note::before { content: "Note: "; color: red; }
        </style></head>
        <body><p class="note" id="p" style="margin:0">Important text</p></body></html>
        ''')
        dl = get_display_list(doc)
        text_cmds = [cmd for cmd in dl if isinstance(cmd, DrawText)]
        all_text = ' '.join(cmd.text for cmd in text_cmds)
        assert 'Note:' in all_text, \
            f"::before content should appear in output, got: {all_text}"
