"""Integration tests: Inline Text Layout and Wrapping.

Verifies that inline content (text, images, inline-block) is laid out
correctly in line boxes, matching real browser behavior.
"""
import pytest
from tests.render_helper import render, find_element, find_all, approx, get_display_list
from rendering.display_list import DrawText


class TestTextRendering:
    """Basic text rendering produces DrawText commands."""

    def test_simple_text_has_draw_commands(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><p id="p" style="margin:0">Hello world</p></body></html>
        ''')
        dl = get_display_list(doc)
        text_cmds = [cmd for cmd in dl if isinstance(cmd, DrawText)]
        assert len(text_cmds) > 0, "Should have at least one DrawText command"
        all_text = ' '.join(cmd.text for cmd in text_cmds)
        assert 'Hello' in all_text
        assert 'world' in all_text

    def test_text_positioned_within_container(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><div style="padding:20px"><p id="p" style="margin:0">Text</p></div></body></html>
        ''')
        p = find_element(doc, tag='p')
        assert hasattr(p, 'line_boxes'), "P should have line_boxes"
        if p.line_boxes:
            first_item = p.line_boxes[0].items[0]
            assert first_item.x >= 20.0, \
                f"Text should be offset by padding, x={first_item.x}"

    def test_text_transform_uppercase_affects_draw_text(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><p style="margin:0; text-transform:uppercase">Hello world</p></body></html>
        ''')
        dl = get_display_list(doc)
        text_cmds = [cmd for cmd in dl if isinstance(cmd, DrawText)]
        all_text = ' '.join(cmd.text for cmd in text_cmds)
        assert 'HELLO' in all_text
        assert 'WORLD' in all_text


class TestLineWrapping:
    """Text wraps at container boundary."""

    def test_long_text_creates_multiple_lines(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="d" style="width:100px; font-size:16px">
            This is a longer sentence that should definitely wrap across multiple lines in a narrow container
          </div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert hasattr(d, 'line_boxes'), "Div should have line_boxes"
        assert len(d.line_boxes) > 1, \
            f"Should wrap into multiple lines, got {len(d.line_boxes)}"

    def test_lines_stack_vertically(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="d" style="width:100px; font-size:16px">
            word1 word2 word3 word4 word5 word6 word7 word8
          </div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        if len(d.line_boxes) >= 2:
            assert d.line_boxes[1].y > d.line_boxes[0].y, \
                "Second line should be below first"


class TestTextAlign:
    """text-align affects horizontal position of inline content."""

    def test_text_align_center(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="d" style="width:500px; text-align:center; font-size:16px">Hi</div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        if d.line_boxes and d.line_boxes[0].items:
            first_item = d.line_boxes[0].items[0]
            # "Hi" is short, so it should be centered — x should be > 100
            assert first_item.x > 100, \
                f"Centered text should be offset, x={first_item.x}"

    def test_text_align_right(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="d" style="width:500px; text-align:right; font-size:16px">Hi</div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        if d.line_boxes and d.line_boxes[0].items:
            first_item = d.line_boxes[0].items[0]
            # Right-aligned: text should be near right edge
            assert first_item.x > 400, \
                f"Right-aligned text should be near right edge, x={first_item.x}"


class TestWhiteSpace:
    """white-space property affects wrapping."""

    def test_nowrap_single_line(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="d" style="width:100px; white-space:nowrap; font-size:16px">
            This is a long sentence that should not wrap at all
          </div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert hasattr(d, 'line_boxes')
        assert len(d.line_boxes) == 1, \
            f"nowrap should produce 1 line, got {len(d.line_boxes)}"

    def test_pre_preserves_newlines(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <pre id="d" style="white-space:pre; font-size:16px; margin:0">line1
line2
line3</pre>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert hasattr(d, 'line_boxes')
        assert len(d.line_boxes) == 3, \
            f"pre should preserve 3 lines, got {len(d.line_boxes)}"


class TestBrElement:
    """<br> forces a line break."""

    def test_br_creates_new_line(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="d" style="width:500px; font-size:16px">
            Line one<br>Line two
          </div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert len(d.line_boxes) >= 2, \
            f"<br> should create at least 2 lines, got {len(d.line_boxes)}"


class TestInlineElements:
    """Inline elements (span, b, em, a) flow within text."""

    def test_bold_text_renders(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><p id="p" style="margin:0">Normal <b>bold</b> text</p></body></html>
        ''')
        dl = get_display_list(doc)
        text_cmds = [cmd for cmd in dl if isinstance(cmd, DrawText)]
        bold_cmds = [cmd for cmd in text_cmds if cmd.weight in ('bold', '700')]
        assert len(bold_cmds) > 0, "Should have bold text commands"

    def test_link_inherits_color(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><p style="margin:0"><a href="#" id="link">click me</a></p></body></html>
        ''')
        link = find_element(doc, id_name='link')
        # UA stylesheet sets a { color: blue; }
        assert link.style.get('color') == 'blue'


class TestFontSize:
    """Font size affects text measurement and line height."""

    def test_larger_font_produces_taller_lines(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="small" style="font-size:12px">Small text</div>
          <div id="large" style="font-size:48px">Large text</div>
        </body></html>
        ''')
        small = find_element(doc, id_name='small')
        large = find_element(doc, id_name='large')
        small_h = small.box.content_height
        large_h = large.box.content_height
        assert large_h > small_h, \
            f"48px text should be taller than 12px (small={small_h}, large={large_h})"


class TestLineHeight:
    """line-height controls spacing between lines."""

    def test_line_height_multiplier(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="tight" style="width:100px; font-size:16px; line-height:1">
            word1 word2 word3 word4 word5 word6
          </div>
          <div id="loose" style="width:100px; font-size:16px; line-height:3">
            word1 word2 word3 word4 word5 word6
          </div>
        </body></html>
        ''')
        tight = find_element(doc, id_name='tight')
        loose = find_element(doc, id_name='loose')
        assert loose.box.content_height > tight.box.content_height, \
            "line-height:3 should be taller than line-height:1"


class TestInlineBlock:
    """display:inline-block participates in inline flow."""

    def test_inline_block_has_width_height(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="parent" style="width:500px">
            Text before
            <span id="ib" style="display:inline-block; width:100px; height:50px; background-color:red">box</span>
            Text after
          </div>
        </body></html>
        ''')
        parent = find_element(doc, id_name='parent')
        # The inline-block should appear in the line boxes
        assert hasattr(parent, 'line_boxes'), "Parent should have line_boxes"
        found_ib = False
        for line in parent.line_boxes:
            for item in line.items:
                if item.type == 'INLINE-BLOCK' and item.width >= 100:
                    found_ib = True
        assert found_ib, "Should find inline-block item in line boxes"
