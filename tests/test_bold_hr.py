"""Tests for <b> bold tag and <hr> horizontal rule tag.

Covers three levels:
  1. CSS cascade — correct computed styles
  2. Block layout — correct BoxModel dimensions
  3. Display list — correct draw commands emitted
"""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import pytest
from html.parser import parse as parse_html
from html.dom import Document, Element, Text

UA = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                  'ua', 'user-agent.css')


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def parse(html: str) -> Document:
    return parse_html(html)


def bind(doc, vw=980, vh=600):
    from css.cascade import bind as _bind
    _bind(doc, UA, viewport_width=vw, viewport_height=vh)
    return doc


def find(node, tag: str):
    """BFS: first element with given tag."""
    q = list(node.children)
    while q:
        n = q.pop(0)
        if isinstance(n, Element) and n.tag == tag:
            return n
        q.extend(getattr(n, 'children', []))
    return None


def make_el(tag='div', style=None, children=None):
    """Create a bare Element with a style dict (no cascade needed)."""
    el = Element(tag)
    el.style = dict(style or {})
    el.attributes = {}
    for ch in (children or []):
        ch.parent = el
        el.children.append(ch)
    return el


def do_block_layout(node, width=800.0):
    from layout.box import BoxModel
    from layout.block import BlockLayout
    from layout.context import LayoutContext
    container = BoxModel()
    container.x = 0.0
    container.y = 0.0
    container.content_width = width
    container.content_height = 0.0
    return BlockLayout().layout(node, container, LayoutContext())


def build_display_list(doc):
    """Run full layout + display list generation (no PyQt6 for block-only docs)."""
    import layout as _layout
    return _layout.layout(doc)


# ===========================================================================
# 1.  <b> BOLD TAG — CSS CASCADE
# ===========================================================================

class TestBoldCascade:
    """The UA stylesheet must give <b> font-weight:bold via CSS cascade."""

    def test_b_font_weight_bold(self):
        doc = bind(parse('<b>text</b>'))
        b = find(doc, 'b')
        assert b is not None
        assert b.style.get('font-weight') in ('bold', '700')

    def test_b_display_inline(self):
        doc = bind(parse('<b>text</b>'))
        b = find(doc, 'b')
        assert b.style.get('display') == 'inline'

    def test_b_inherits_font_size_from_parent(self):
        """<b> must inherit font-size from its containing block."""
        doc = bind(parse('<p style="font-size:20px"><b>big bold</b></p>'))
        b = find(doc, 'b')
        assert b.style.get('font-size') == '20px'
        assert b.style.get('font-weight') in ('bold', '700')

    def test_b_inherits_color_from_parent(self):
        doc = bind(parse('<p style="color:red"><b>red bold</b></p>'))
        b = find(doc, 'b')
        assert b.style.get('color') == 'red'

    def test_strong_also_bold(self):
        """<strong> must behave identically to <b>."""
        doc = bind(parse('<strong>text</strong>'))
        strong = find(doc, 'strong')
        assert strong.style.get('font-weight') in ('bold', '700')

    def test_sibling_paragraph_not_bold(self):
        """A <p> next to a <b> must not be bold."""
        doc = bind(parse('<b>bold</b><p>normal</p>'))
        p = find(doc, 'p')
        fw = p.style.get('font-weight', 'normal')
        assert fw not in ('bold', '700')

    def test_nested_b_still_bold(self):
        """Nested <b><b> must be bold (idempotent)."""
        doc = bind(parse('<b><b>double</b></b>'))
        inner = None
        # find second b
        outer = find(doc, 'b')
        if outer:
            inner = find(outer, 'b')
        assert inner is not None
        assert inner.style.get('font-weight') in ('bold', '700')

    def test_b_inside_div_bold(self):
        doc = bind(parse('<div><b>bold</b></div>'))
        b = find(doc, 'b')
        assert b.style.get('font-weight') in ('bold', '700')

    def test_author_css_can_override_bold(self):
        """Author CSS font-weight:normal must override UA bold on <b>."""
        html = '<style>b { font-weight: normal; }</style><b>not bold</b>'
        doc = bind(parse(html))
        b = find(doc, 'b')
        assert b.style.get('font-weight') == 'normal'

    def test_inline_style_bold_on_span(self):
        """Inline style font-weight:bold on any element must work."""
        doc = bind(parse('<span style="font-weight:bold">bold span</span>'))
        span = find(doc, 'span')
        assert span.style.get('font-weight') in ('bold', '700')


# ===========================================================================
# 2.  <b> BOLD TAG — INLINE ITEM FONT WEIGHT
# ===========================================================================

def _fake_measure(text, family, size_px, weight, italic):
    """Stub text measurement so tests don't need PyQt6."""
    return (len(text) * 8.0, 16.0)


class TestBoldInlineItems:
    """Inline layout must produce InlineItems with font_weight='bold' for text
    that is a descendant of a <b> element."""

    def _collect_items(self, html: str, container_width: float = 800.0):
        """Return all InlineItems from the body element (inline root)."""
        from unittest.mock import patch
        from layout.inline import _collect_inline_items
        doc = bind(parse(html))
        body = find(doc, 'body')
        with patch('layout.inline.measure_text', side_effect=_fake_measure):
            return _collect_inline_items(body, container_width)

    def test_text_inside_b_has_bold_weight(self):
        items = self._collect_items('<b>hello</b>')
        word_items = [it for it in items if it.text == 'hello']
        assert word_items, "expected an InlineItem for 'hello'"
        assert word_items[0].font_weight in ('bold', '700')

    def test_text_outside_b_has_normal_weight(self):
        items = self._collect_items('normal <b>bold</b> normal')
        # 'normal' appears twice (before and after <b>); neither should be bold
        normal_items = [it for it in items if it.text == 'normal']
        assert normal_items
        assert normal_items[0].font_weight not in ('bold', '700')

    def test_b_word_weight_matches_element_style(self):
        """Every word inside <b> must have font_weight matching <b>.style."""
        from unittest.mock import patch
        from layout.inline import _collect_inline_items
        doc = bind(parse('<b>one two three</b>'))
        body = find(doc, 'body')
        with patch('layout.inline.measure_text', side_effect=_fake_measure):
            items = _collect_inline_items(body, 800.0)
        word_items = [it for it in items if it.text.strip()]
        assert word_items
        for item in word_items:
            assert item.font_weight in ('bold', '700'), (
                f"word '{item.text}' has font_weight={item.font_weight!r}"
            )


# ===========================================================================
# 3.  <hr> TAG — CSS CASCADE
# ===========================================================================

class TestHrCascade:
    """UA stylesheet must give <hr> a visible top border and block display."""

    def test_hr_display_block(self):
        doc = bind(parse('<hr>'))
        hr = find(doc, 'hr')
        assert hr.style.get('display') == 'block'

    def test_hr_border_top_style_solid(self):
        doc = bind(parse('<hr>'))
        hr = find(doc, 'hr')
        # Individual longhand takes priority
        top_style = hr.style.get('border-top-style') or hr.style.get('border-style')
        assert top_style == 'solid', f"expected solid, got {top_style!r}"

    def test_hr_border_top_width_nonzero(self):
        """border-top-width must resolve to a positive pixel value, not 'medium'."""
        from layout.text import _parse_px
        doc = bind(parse('<hr>'))
        hr = find(doc, 'hr')
        w_str = hr.style.get('border-top-width', '0')
        w_px = _parse_px(w_str)
        assert w_px > 0, (
            f"border-top-width={w_str!r} resolves to {w_px}px — <hr> will be invisible"
        )

    def test_hr_border_top_color_set(self):
        doc = bind(parse('<hr>'))
        hr = find(doc, 'hr')
        color = hr.style.get('border-top-color') or hr.style.get('border-color')
        assert color and color not in ('currentcolor', 'transparent', ''), (
            f"expected a real color, got {color!r}"
        )

    def test_hr_other_borders_none(self):
        """Only the top border should be solid; sides/bottom should be none."""
        doc = bind(parse('<hr>'))
        hr = find(doc, 'hr')
        for side in ('right', 'bottom', 'left'):
            style = hr.style.get(f'border-{side}-style', 'none')
            assert style in ('none', '', 'hidden'), (
                f"border-{side}-style should be none, got {style!r}"
            )

    def test_hr_has_margin_top(self):
        from layout.text import _parse_px
        doc = bind(parse('<hr>'))
        hr = find(doc, 'hr')
        mt = _parse_px(hr.style.get('margin-top', '0'))
        assert mt > 0, "expected positive margin-top on <hr>"

    def test_hr_has_margin_bottom(self):
        from layout.text import _parse_px
        doc = bind(parse('<hr>'))
        hr = find(doc, 'hr')
        mb = _parse_px(hr.style.get('margin-bottom', '0'))
        assert mb > 0, "expected positive margin-bottom on <hr>"


# ===========================================================================
# 4.  <hr> TAG — BLOCK LAYOUT (BoxModel)
# ===========================================================================

class TestHrLayout:
    """BlockLayout must give <hr> a non-zero border box height."""

    def _layout_hr(self, width=800.0):
        """Layout a bare <hr> element with the correct computed styles."""
        from layout.text import _parse_px
        doc = bind(parse('<hr>'))
        hr = find(doc, 'hr')
        box = do_block_layout(hr, width)
        return box, hr

    def test_hr_fills_container_width(self):
        box, _ = self._layout_hr(width=600.0)
        # border box width = content_width + border + padding
        total_w = box.content_width + box.border.left + box.border.right + box.padding.left + box.padding.right
        assert total_w <= 600.0

    def test_hr_border_box_has_positive_height(self):
        """The border box of <hr> must be taller than zero (due to the top border)."""
        box, _ = self._layout_hr()
        border_box_h = box.content_height + box.border.top + box.border.bottom
        assert border_box_h > 0, (
            f"<hr> border-box height is {border_box_h}px — element is invisible"
        )

    def test_hr_border_top_width_parsed(self):
        """box.border.top must be 1px (from border-top-width: 1px in UA)."""
        box, _ = self._layout_hr()
        assert box.border.top == pytest.approx(1.0), (
            f"expected border.top=1.0, got {box.border.top}"
        )

    def test_hr_border_other_sides_zero(self):
        box, _ = self._layout_hr()
        assert box.border.right == 0.0
        assert box.border.bottom == 0.0
        assert box.border.left == 0.0

    def test_hr_margin_top_applied(self):
        box, _ = self._layout_hr()
        assert box.margin.top > 0

    def test_hr_margin_bottom_applied(self):
        box, _ = self._layout_hr()
        assert box.margin.bottom > 0


# ===========================================================================
# 5.  <hr> TAG — DISPLAY LIST
# ===========================================================================

class TestHrDisplayList:
    """The display list must contain a DrawBorder command for <hr>."""

    def test_hr_emits_draw_border(self):
        from rendering.display_list import DrawBorder
        doc = bind(parse('<body><hr></body>'))
        dl = build_display_list(doc)
        borders = [cmd for cmd in dl.commands if isinstance(cmd, DrawBorder)]
        assert borders, "expected at least one DrawBorder command for <hr>"

    def test_hr_draw_border_has_solid_top(self):
        from rendering.display_list import DrawBorder
        doc = bind(parse('<body><hr></body>'))
        dl = build_display_list(doc)
        borders = [cmd for cmd in dl.commands if isinstance(cmd, DrawBorder)]
        assert borders
        # styles tuple is (top, right, bottom, left)
        top_style = borders[0].styles[0]
        assert top_style == 'solid', f"expected solid top border, got {top_style!r}"

    def test_hr_draw_border_non_none_sides_only_top(self):
        """Only the top side of the border should be non-none."""
        from rendering.display_list import DrawBorder
        doc = bind(parse('<body><hr></body>'))
        dl = build_display_list(doc)
        borders = [cmd for cmd in dl.commands if isinstance(cmd, DrawBorder)]
        assert borders
        top, right, bottom, left = borders[0].styles
        assert top == 'solid'
        assert right in ('none', '', 'hidden')
        assert bottom in ('none', '', 'hidden')
        assert left in ('none', '', 'hidden')

    def test_paragraph_then_hr_then_paragraph_order(self):
        """<hr> between paragraphs must produce a DrawBorder between DrawText commands."""
        try:
            from PyQt6.QtWidgets import QApplication  # noqa: F401
        except ImportError:
            pytest.skip("PyQt6 not available — skipping display-list ordering test")

        from rendering.display_list import DrawBorder, DrawText
        doc = bind(parse('<body><p>above</p><hr><p>below</p></body>'))
        dl = build_display_list(doc)
        cmds = dl.commands

        border_idx = next((i for i, c in enumerate(cmds) if isinstance(c, DrawBorder)), None)
        assert border_idx is not None, "no DrawBorder found"

        texts = [(i, c) for i, c in enumerate(cmds) if isinstance(c, DrawText)]
        above_idx = next((i for i, c in texts if 'above' in c.text), None)
        below_idx = next((i for i, c in texts if 'below' in c.text), None)

        if above_idx is not None and below_idx is not None:
            assert above_idx < border_idx < below_idx, (
                f"expected above({above_idx}) < border({border_idx}) < below({below_idx})"
            )
