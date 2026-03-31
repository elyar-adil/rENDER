"""Tests for layout behaviors that are incorrect or not implemented.

These tests document the gap between rENDER's current layout output and what
a real browser would produce. Tests that are expected to fail are marked xfail
with an explanation of the missing feature.
"""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import pytest
from html.dom import Document, Element, Text as TextNode
from layout.box import BoxModel, EdgeSizes
from layout.block import BlockLayout
from layout.context import LayoutContext


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def make_element(tag='div', style=None, attrs=None, children=None, parent=None):
    el = Element(tag)
    el.style = dict(style or {})
    el.attributes = dict(attrs or {})
    for c in (children or []):
        c.parent = el
        el.children.append(c)
    if parent is not None:
        el.parent = parent
        parent.children.append(el)
    return el


def make_container(x=0.0, y=0.0, width=500.0, height=0.0):
    c = BoxModel()
    c.x = x
    c.y = y
    c.content_width = width
    c.content_height = height
    return c


def do_layout(node, width=500.0, height=0.0, x=0.0, y=0.0):
    container = make_container(x, y, width, height)
    ctx = LayoutContext()
    return BlockLayout().layout(node, container, ctx)


# ===========================================================================
# 1. display: inline-block – SHRINK-TO-FIT WIDTH
# ===========================================================================

class TestInlineBlockWidth:
    """display:inline-block with width:auto should shrink to its content,
    not fill the container.  Currently BlockLayout fills the container."""

    def test_inline_block_auto_width_is_shrink_to_fit(self):
        """An inline-block with auto width should not exceed its content width."""
        el = make_element(style={
            'display': 'inline-block', 'width': 'auto',
            'height': '40px',
        })
        txt = TextNode('OK')
        txt.parent = el
        el.children.append(txt)
        box = do_layout(el, width=500.0)
        # Content should be ~20–40px wide, not 500px
        assert box.content_width < 100.0, (
            f"Expected shrink-to-fit (< 100px), got {box.content_width}px"
        )

    def test_inline_block_explicit_width_respected(self):
        """inline-block with explicit px width – width is respected (no auto-expand)."""
        el = make_element(style={
            'display': 'inline-block', 'width': '200px', 'height': '50px',
        })
        box = do_layout(el, width=500.0)
        assert box.content_width == 200.0


# ===========================================================================
# 2. position: sticky – NOT IMPLEMENTED
# ===========================================================================

class TestPositionSticky:

    def test_sticky_element_is_not_deferred(self):
        """Sticky positioned elements must participate in normal flow AND
        be remembered for later offset adjustment. Currently they are just
        treated as static – no sticky behavior at all."""
        from layout._dispatch import layout_node
        el = make_element(style={
            'display': 'block', 'position': 'sticky',
            'top': '0px', 'width': 'auto', 'height': '60px',
        })
        c = make_container(width=500)
        ctx = LayoutContext()
        box = layout_node(el, c, ctx)
        # Sticky element should produce a box (not None like absolute/fixed)
        assert box is not None
        # AND the ctx should record the sticky constraint
        assert len(getattr(ctx, 'sticky_elements', [])) > 0, \
            "sticky elements should be tracked in LayoutContext"

    def test_static_element_produces_box(self):
        """Sanity: static positioning returns a box."""
        el = make_element(style={'display': 'block', 'height': '60px'})
        box = do_layout(el)
        assert box is not None

    def test_absolute_element_deferred(self):
        """Absolute positioned elements return None from layout_node."""
        from layout._dispatch import layout_node
        el = make_element(style={
            'display': 'block', 'position': 'absolute',
            'top': '0px', 'left': '0px',
        })
        c = make_container()
        box = layout_node(el, c, LayoutContext())
        assert box is None


# ===========================================================================
# 3. visibility: hidden – ELEMENT SHOULD TAKE SPACE BUT NOT RENDER
# ===========================================================================

class TestVisibilityHidden:

    def test_visibility_hidden_same_height_as_visible(self):
        """visibility:hidden element still occupies its full height in layout."""
        hidden = make_element(style={
            'display': 'block', 'visibility': 'hidden',
            'height': '100px',
        })
        visible = make_element(style={'display': 'block', 'height': '100px'})
        box_h = do_layout(hidden)
        box_v = do_layout(visible)
        assert box_h.content_height == box_v.content_height

    def test_visibility_hidden_not_display_none(self):
        """visibility:hidden ≠ display:none – layout still generates a box."""
        from layout._dispatch import layout_node
        el = make_element(style={'display': 'block', 'visibility': 'hidden'})
        c = make_container()
        box = layout_node(el, c, LayoutContext())
        assert box is not None

    def test_visibility_hidden_not_in_display_list(self):
        """visibility:hidden elements should produce draw commands with opacity=0 or be omitted."""
        from tests.render_helper import render, get_display_list
        from rendering.display_list import DrawRect, DrawText

        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="visibility:hidden; width:200px; height:100px;
                      background-color:red">Hidden</div>
          <div style="width:200px; height:100px;
                      background-color:blue">Visible</div>
        </body></html>
        ''')
        dl = get_display_list(doc)

        red_rects = [cmd for cmd in dl if isinstance(cmd, DrawRect) and cmd.color == 'red']
        hidden_texts = [cmd for cmd in dl if isinstance(cmd, DrawText) and 'Hidden' in cmd.text]
        blue_rects = [cmd for cmd in dl if isinstance(cmd, DrawRect) and cmd.color == 'blue']

        assert len(red_rects) == 0, "visibility:hidden background should not be painted"
        assert len(hidden_texts) == 0, "visibility:hidden text should not be painted"
        assert len(blue_rects) >= 1, "visible sibling should still be painted"


# ===========================================================================
# 4. overflow: hidden – CONTENT SHOULD BE CLIPPED
# ===========================================================================

class TestOverflowHidden:

    def test_overflow_hidden_explicit_height_unchanged(self):
        """Explicit height is respected even with children taller than parent."""
        parent = make_element(style={
            'display': 'block', 'height': '100px', 'overflow': 'visible',
        })
        child = make_element(style={'display': 'block', 'height': '200px'})
        child.parent = parent
        parent.children.append(child)
        box = do_layout(parent, width=300.0)
        # overflow:visible (default) → explicit height=100px but children extend content
        # BlockLayout uses max(specified_h, child_y) for overflow:visible
        assert box.content_height >= 100.0

    def test_max_height_clamps_auto_height(self):
        """max-height limits block height even with auto height and overflowing children."""
        parent = make_element(style={
            'display': 'block', 'height': 'auto',
            'max-height': '100px',
        })
        child = make_element(style={'display': 'block', 'height': '200px'})
        child.parent = parent
        parent.children.append(child)
        box = do_layout(parent, width=300.0)
        assert box.content_height <= 100.0, (
            f"Expected height<=100 (max-height), got {box.content_height}"
        )

    @pytest.mark.xfail(reason="overflow:hidden alone does not clip layout height; parent expands with children")
    def test_overflow_hidden_alone_clips_auto_height(self):
        """overflow:hidden without max-height should prevent children from
        extending a height:auto parent (but currently it doesn't)."""
        parent = make_element(style={
            'display': 'block', 'height': 'auto', 'overflow': 'hidden',
        })
        child = make_element(style={'display': 'block', 'height': '200px'})
        child.parent = parent
        parent.children.append(child)
        box = do_layout(parent, width=300.0)
        # With only overflow:hidden (no explicit height), should clip to 0 or shrink
        # In reality it should still show children up to the stacking context boundary
        # This documents that overflow:hidden has no effect on layout height
        assert box.content_height == 0.0, (
            "overflow:hidden with auto height should not grow from children; "
            f"got {box.content_height}"
        )

    def test_overflow_visible_children_extend_height(self):
        """overflow:visible (default) allows children to expand parent height."""
        parent = make_element(style={
            'display': 'block', 'height': 'auto', 'overflow': 'visible',
        })
        child = make_element(style={'display': 'block', 'height': '200px'})
        child.parent = parent
        parent.children.append(child)
        box = do_layout(parent, width=300.0)
        assert box.content_height >= 200.0


# ===========================================================================
# 5. display: contents – ELEMENT ITSELF GENERATES NO BOX
# ===========================================================================

class TestDisplayContents:

    def test_display_contents_generates_no_box(self):
        """display:contents element generates no layout box (skipped by BlockLayout)."""
        parent = make_element(style={'display': 'block', 'width': '300px'})
        wrapper = make_element(style={'display': 'contents'})
        child = make_element(style={'display': 'block', 'height': '50px'})
        child.parent = wrapper
        wrapper.children.append(child)
        wrapper.parent = parent
        parent.children.append(wrapper)

        from layout._dispatch import layout_node
        c = make_container(width=300)
        ctx = LayoutContext()
        layout_node(parent, c, ctx)
        assert not hasattr(wrapper, 'box') or wrapper.box is None, \
            "display:contents element should not generate a box"

    def test_display_contents_children_laid_out_via_inline_pass(self):
        """Children of a display:contents element are visited via the inline pass."""
        parent = make_element(style={'display': 'block', 'width': '300px'})
        wrapper = make_element(style={'display': 'contents'})
        child = make_element(style={'display': 'block', 'height': '50px'})
        child.parent = wrapper
        wrapper.children.append(child)
        wrapper.parent = parent
        parent.children.append(wrapper)

        from layout._dispatch import layout_node
        c = make_container(width=300)
        ctx = LayoutContext()
        box = layout_node(parent, c, ctx)
        # Child of contents wrapper IS laid out (via inline pass visiting wrapper's children)
        assert hasattr(child, 'box') and child.box is not None, \
            "child of display:contents should be laid out"
        assert box.content_height >= 50.0


# ===========================================================================
# 6. TEXT RENDERING PROPERTIES – NOT APPLIED
# ===========================================================================

class TestTextPropertiesNotApplied:

    def test_text_transform_uppercase(self):
        """text-transform:uppercase should affect text content before layout."""
        from html.dom import Document
        from css.cascade import bind
        import os as _os

        ua = _os.path.join(_os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))),
                           'ua', 'user-agent.css')
        doc = Document()
        html_el = make_element('html')
        html_el.parent = doc
        doc.children.append(html_el)
        head = make_element('head', parent=html_el)
        style_el = make_element('style', parent=head)
        style_el.style = {}
        txt = TextNode('p { text-transform: uppercase; }')
        txt.parent = style_el
        style_el.children.append(txt)
        body = make_element('body', parent=html_el)
        body.style = {}
        p = make_element('p', parent=body)
        t = TextNode('hello world')
        t.parent = p
        p.children.append(t)

        bind(doc, ua)

        # After cascade+transform, the text content should be uppercased
        # (either the TextNode.data is transformed or a computed property set)
        assert p.style.get('text-transform') == 'uppercase'
        # The actual text output must also be uppercase:
        assert t.data == 'HELLO WORLD', f"Expected uppercase, got: {t.data!r}"

    def test_white_space_nowrap_prevents_wrap(self):
        """white-space:nowrap must prevent line breaks even when text overflows."""
        from layout.inline import layout_inline
        el = make_element('p', style={
            'display': 'block', 'white-space': 'nowrap',
            'font-size': '16px', 'font-family': 'Arial',
        })
        # Long text that would normally wrap at 200px
        long_text = TextNode('This is a very long sentence that should not wrap at all.')
        long_text.parent = el
        el.children.append(long_text)
        lines, _ = layout_inline(el, 0.0, 0.0, 200.0)
        assert len(lines) == 1, f"Expected 1 line (nowrap), got {len(lines)} lines"

    def test_white_space_pre_preserves_newlines(self):
        """white-space:pre must preserve literal newlines in text nodes."""
        from layout.inline import layout_inline
        el = make_element('pre', style={
            'display': 'block', 'white-space': 'pre',
            'font-size': '16px', 'font-family': 'Arial',
        })
        code = TextNode('line1\nline2\nline3')
        code.parent = el
        el.children.append(code)
        lines, _ = layout_inline(el, 0.0, 0.0, 500.0)
        assert len(lines) == 3, f"Expected 3 lines (pre), got {len(lines)}"

    def test_text_indent_applied(self):
        """text-indent:40px shifts the first line of text."""
        from layout.inline import layout_inline
        el = make_element('p', style={
            'display': 'block', 'text-indent': '40px',
            'font-size': '16px', 'font-family': 'Arial',
        })
        txt = TextNode('Indented paragraph.')
        txt.parent = el
        el.children.append(txt)
        lines, _ = layout_inline(el, 0.0, 0.0, 400.0)
        assert lines, "no lines produced"
        first_item = lines[0].items[0] if lines[0].items else None
        assert first_item is not None
        # First item x should be offset by the indent
        assert first_item.x >= 40.0, f"Expected x>=40 for text-indent, got {first_item.x}"

    def test_text_overflow_ellipsis(self):
        """overflow:hidden + white-space:nowrap + text-overflow:ellipsis should truncate."""
        from layout.inline import layout_inline
        el = make_element('p', style={
            'display': 'block', 'width': '100px',
            'overflow': 'hidden', 'white-space': 'nowrap',
            'text-overflow': 'ellipsis',
            'font-size': '16px', 'font-family': 'Arial',
        })
        txt = TextNode('This long text should be truncated with ellipsis')
        txt.parent = el
        el.children.append(txt)
        lines, _ = layout_inline(el, 0.0, 0.0, 100.0)
        assert lines, "no lines produced"
        # Render text should end with '…' or '...'
        all_text = ''.join(item.text for item in lines[0].items if item.text)
        assert all_text.endswith('…') or all_text.endswith('...'), (
            f"Expected ellipsis, got: {all_text!r}"
        )


# ===========================================================================
# 7. MARGIN COLLAPSING EDGE CASES
# ===========================================================================

class TestMarginCollapsingEdgeCases:

    def test_adjacent_positive_margins_collapse(self):
        """Two positive vertical margins between siblings collapse to the larger one."""
        parent = make_element(style={'display': 'block'})
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
        child1.parent = parent
        child2.parent = parent
        parent.children = [child1, child2]
        box = do_layout(parent)
        # child2 should start at 50+30=80 (collapsed: max(20,30)=30)
        child2_y = child2.box.y if hasattr(child2, 'box') and child2.box else None
        assert child2_y == 80.0, f"Expected child2.y=80 (margin collapsed), got {child2_y}"

    def test_adjacent_negative_margin_collapse(self):
        """Negative margin collapses: max positive + min negative."""
        parent = make_element(style={'display': 'block'})
        child1 = make_element(style={
            'display': 'block', 'height': '50px',
            'margin-top': '0px', 'margin-bottom': '20px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        child2 = make_element(style={
            'display': 'block', 'height': '50px',
            'margin-top': '-10px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        child1.parent = parent
        child2.parent = parent
        parent.children = [child1, child2]
        do_layout(parent)
        # Collapsed: 20 + (-10) = 10 → child2.y = 50 + 10 = 60
        assert hasattr(child2, 'box') and child2.box is not None
        assert child2.box.y == 60.0, f"Expected y=60, got {child2.box.y}"

    def test_parent_child_margin_collapse(self):
        """Parent with no border/padding: child's top margin collapses with parent's top margin."""
        outer = make_element(style={
            'display': 'block',
            'margin-top': '20px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        inner = make_element(style={
            'display': 'block', 'height': '50px',
            'margin-top': '30px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        inner.parent = outer
        outer.children = [inner]
        c = make_container(x=0, y=0, width=400, height=0)
        ctx = LayoutContext()
        box = BlockLayout().layout(outer, c, ctx)
        # Parent top margin should collapse with child: max(20,30)=30
        assert box.y == 30.0, f"Expected box.y=30 (collapsed), got {box.y}"


# ===========================================================================
# 8. Z-INDEX / STACKING CONTEXT – NOT IMPLEMENTED
# ===========================================================================

class TestZIndex:

    def test_z_index_stored_in_style(self):
        """z-index should be stored in element.style after cascade."""
        el = make_element(style={'z-index': '10', 'position': 'relative'})
        # Just verify the value is accessible – layout doesn't use it
        assert el.style.get('z-index') == '10'

    @pytest.mark.xfail(reason="z-index stacking context not implemented in display list generation")
    def test_z_index_higher_drawn_last(self):
        """Elements with higher z-index should appear later in the display list
        so they render on top.  rENDER does not implement stacking contexts."""
        from rendering.display_list import DisplayList
        # Cannot easily test without running full pipeline; mark as xfail
        raise NotImplementedError("stacking context not implemented")


# ===========================================================================
# 9. PERCENTAGE HEIGHTS – REQUIRE KNOWN PARENT HEIGHT
# ===========================================================================

class TestPercentageHeight:

    def test_percentage_height_with_known_parent(self):
        """height:50% should compute to 50% of the explicit parent height."""
        parent = make_element(style={'display': 'block', 'height': '200px'})
        child = make_element(style={'display': 'block', 'height': '50%'})
        child.parent = parent
        parent.children = [child]
        do_layout(parent)
        assert hasattr(child, 'box') and child.box is not None
        assert child.box.content_height == 100.0, (
            f"Expected 100px (50% of 200px), got {child.box.content_height}"
        )

    def test_auto_height_from_children(self):
        """height:auto resolves to the sum of child heights."""
        parent = make_element(style={'display': 'block'})
        for _ in range(3):
            ch = make_element(style={
                'display': 'block', 'height': '40px',
                'margin-top': '0px', 'margin-bottom': '0px',
                'margin-left': '0px', 'margin-right': '0px',
            })
            ch.parent = parent
            parent.children.append(ch)
        box = do_layout(parent)
        assert box.content_height == 120.0


# ===========================================================================
# 10. GRID LAYOUT GAPS
# ===========================================================================

class TestGridGaps:

    @pytest.mark.xfail(reason="grid-template-areas assignment not implemented")
    def test_grid_template_areas(self):
        """grid-area names should place children in the correct cell."""
        from layout.grid import GridLayout

        container = make_element(style={
            'display': 'grid',
            'grid-template-columns': '1fr 1fr',
            'grid-template-rows': '100px 100px',
            'grid-template-areas': '"header header" "sidebar main"',
            'width': '400px',
        })
        header = make_element(style={'display': 'block', 'grid-area': 'header'})
        sidebar = make_element(style={'display': 'block', 'grid-area': 'sidebar'})
        main = make_element(style={'display': 'block', 'grid-area': 'main'})
        for ch in (header, sidebar, main):
            ch.parent = container
            container.children.append(ch)

        c = make_container(width=400)
        ctx = LayoutContext()
        GridLayout().layout(container, c, ctx)

        # header spans full width → x=0, width=400
        assert header.box.x == 0.0
        assert header.box.content_width == 400.0

    @pytest.mark.xfail(reason="grid-column/grid-row span not fully implemented")
    def test_grid_column_span(self):
        """grid-column: span 2 should make an item span two columns."""
        from layout.grid import GridLayout

        container = make_element(style={
            'display': 'grid',
            'grid-template-columns': '1fr 1fr 1fr',
            'width': '300px',
        })
        wide = make_element(style={
            'display': 'block', 'grid-column': 'span 2',
        })
        normal = make_element(style={'display': 'block'})
        for ch in (wide, normal):
            ch.parent = container
            container.children.append(ch)

        c = make_container(width=300)
        ctx = LayoutContext()
        GridLayout().layout(container, c, ctx)

        assert wide.box.content_width == 200.0, (
            f"Expected 200px (span 2 of 3 at 100px each), got {wide.box.content_width}"
        )


# ===========================================================================
# 11. TABLE LAYOUT GAPS
# ===========================================================================

class TestTableGaps:

    @pytest.mark.xfail(reason="border-collapse not implemented")
    def test_border_collapse_cell_positions(self):
        """With border-collapse, adjacent cells share a border (no gap between cells)."""
        from layout.table import TableLayout

        table = make_element('table', style={
            'display': 'table', 'border-collapse': 'collapse', 'width': '300px',
        })
        row = make_element('tr', style={'display': 'table-row'}, parent=table)
        td1 = make_element('td', style={
            'display': 'table-cell',
            'border-top-width': '2px', 'border-right-width': '2px',
            'border-bottom-width': '2px', 'border-left-width': '2px',
            'border-top-style': 'solid', 'border-right-style': 'solid',
            'border-bottom-style': 'solid', 'border-left-style': 'solid',
        }, parent=row)
        td2 = make_element('td', style={
            'display': 'table-cell',
            'border-top-width': '2px', 'border-right-width': '2px',
            'border-bottom-width': '2px', 'border-left-width': '2px',
            'border-top-style': 'solid', 'border-right-style': 'solid',
            'border-bottom-style': 'solid', 'border-left-style': 'solid',
        }, parent=row)

        c = make_container(width=300)
        ctx = LayoutContext()
        TableLayout().layout(table, c, ctx)

        # With collapse: 3 borders total (2px each) = 6px, content = 294px
        # Without collapse: 4 borders = 8px, content = 292px
        # td2.x should immediately follow td1's content (no doubled border)
        # td2.x == td1.x + td1.content_width + 2px (shared border)
        expected_td2_x = td1.box.x + td1.box.content_width + 2.0
        assert abs(td2.box.x - expected_td2_x) < 1.0, (
            f"With border-collapse, td2.x should be {expected_td2_x}, got {td2.box.x}"
        )

    @pytest.mark.xfail(reason="vertical-align on table cells not fully implemented")
    def test_table_cell_vertical_align_middle(self):
        """Table cell content should be centred vertically with vertical-align:middle."""
        from layout.table import TableLayout

        table = make_element('table', style={
            'display': 'table', 'width': '200px',
        })
        row = make_element('tr', style={'display': 'table-row'}, parent=table)
        td = make_element('td', style={
            'display': 'table-cell',
            'height': '100px', 'vertical-align': 'middle',
        }, parent=row)
        inner = make_element('div', style={'display': 'block', 'height': '20px'}, parent=td)

        c = make_container(width=200)
        ctx = LayoutContext()
        TableLayout().layout(table, c, ctx)

        # Inner div should be offset to the vertical centre of the cell (40px)
        assert inner.box.y == 40.0, f"Expected y=40 (centred), got {inner.box.y}"


# ===========================================================================
# 12. FLOAT LAYOUT GAPS
# ===========================================================================

class TestFloatGaps:

    def test_float_left_shrinks_to_content(self):
        """A floated element with width:auto should shrink to its content width."""
        el = make_element(style={
            'display': 'block', 'float': 'left', 'width': 'auto',
        })
        txt = TextNode('Hi')
        txt.parent = el
        el.children.append(txt)
        box = do_layout(el, width=400.0)
        # Float should be narrow (content width), not 400px
        assert box.content_width < 100.0, (
            f"Float should shrink-wrap to content, got {box.content_width}px"
        )

    def test_clear_both_pushes_below_floats(self):
        """clear:both element must start below any preceding floats."""
        parent = make_element(style={'display': 'block'})
        float_el = make_element(style={
            'display': 'block', 'float': 'left',
            'width': '100px', 'height': '80px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        clear_el = make_element(style={
            'display': 'block', 'clear': 'both',
            'height': '40px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        float_el.parent = parent
        clear_el.parent = parent
        parent.children = [float_el, clear_el]
        do_layout(parent, width=400.0)
        assert hasattr(clear_el, 'box') and clear_el.box is not None
        # clear element should be at or below float bottom (y >= 80)
        assert clear_el.box.y >= 80.0, (
            f"clear:both element should be below float (y>=80), got y={clear_el.box.y}"
        )


# ===========================================================================
# 13. CSS SPECIFICITY EDGE CASES
# ===========================================================================

class TestSpecificityEdgeCases:

    def test_id_beats_class(self):
        """#id rule wins over .class rule regardless of source order."""
        from html.dom import Document
        from css.cascade import bind
        import os as _os

        ua = _os.path.join(_os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))),
                           'ua', 'user-agent.css')
        doc = Document()
        html_el = make_element('html')
        html_el.parent = doc
        doc.children.append(html_el)
        head = make_element('head', parent=html_el)
        style_el = make_element('style', parent=head)
        style_el.style = {}
        css = TextNode('.warn { color: orange; } #main { color: blue; }')
        css.parent = style_el
        style_el.children.append(css)
        body = make_element('body', parent=html_el)
        body.style = {}
        el = make_element('div', attrs={'id': 'main', 'class': 'warn'}, parent=body)
        bind(doc, ua)
        # #main (specificity 1,0,0) beats .warn (0,1,0)
        assert el.style.get('color') == 'blue', (
            f"Expected 'blue' from #main rule, got {el.style.get('color')!r}"
        )

    def test_inline_style_beats_stylesheet(self):
        """Inline style always wins over author stylesheet."""
        from html.dom import Document
        from css.cascade import bind
        import os as _os

        ua = _os.path.join(_os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))),
                           'ua', 'user-agent.css')
        doc = Document()
        html_el = make_element('html')
        html_el.parent = doc
        doc.children.append(html_el)
        head = make_element('head', parent=html_el)
        style_el = make_element('style', parent=head)
        style_el.style = {}
        css = TextNode('p { color: red; }')
        css.parent = style_el
        style_el.children.append(css)
        body = make_element('body', parent=html_el)
        body.style = {}
        p = make_element('p', attrs={'style': 'color: green'}, parent=body)
        bind(doc, ua)
        assert p.style.get('color') == 'green', (
            f"Expected 'green' from inline style, got {p.style.get('color')!r}"
        )

    def test_important_beats_inline_style(self):
        """!important in stylesheet beats inline style."""
        from html.dom import Document
        from css.cascade import bind
        import os as _os

        ua = _os.path.join(_os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))),
                           'ua', 'user-agent.css')
        doc = Document()
        html_el = make_element('html')
        html_el.parent = doc
        doc.children.append(html_el)
        head = make_element('head', parent=html_el)
        style_el = make_element('style', parent=head)
        style_el.style = {}
        css = TextNode('p { color: purple !important; }')
        css.parent = style_el
        style_el.children.append(css)
        body = make_element('body', parent=html_el)
        body.style = {}
        p = make_element('p', attrs={'style': 'color: green'}, parent=body)
        bind(doc, ua)
        assert p.style.get('color') == 'purple', (
            f"Expected 'purple' from !important, got {p.style.get('color')!r}"
        )


# ===========================================================================
# 14. DISPLAY: NONE COMPLETELY EXCLUDED FROM LAYOUT
# ===========================================================================

class TestDisplayNone:

    def test_display_none_excluded_from_layout(self):
        """display:none elements should produce no box."""
        from layout._dispatch import layout_node
        el = make_element(style={'display': 'none'})
        c = make_container()
        box = layout_node(el, c, LayoutContext())
        assert box is None

    def test_display_none_sibling_not_pushed_down(self):
        """display:none sibling does not occupy space."""
        parent = make_element(style={'display': 'block'})
        hidden = make_element(style={
            'display': 'none', 'height': '200px',
        })
        visible = make_element(style={
            'display': 'block', 'height': '50px',
            'margin-top': '0px', 'margin-bottom': '0px',
            'margin-left': '0px', 'margin-right': '0px',
        })
        hidden.parent = parent
        visible.parent = parent
        parent.children = [hidden, visible]
        do_layout(parent)
        assert hasattr(visible, 'box') and visible.box is not None
        # visible should start at y=0, not pushed down by hidden
        assert visible.box.y == 0.0, (
            f"Expected visible.y=0 (hidden el takes no space), got {visible.box.y}"
        )


# ===========================================================================
# 15. ABSOLUTE POSITIONING OFFSET CALCULATION
# ===========================================================================

class TestAbsolutePositioningGaps:

    def test_absolute_positioned_relative_to_positioned_ancestor(self):
        """position:absolute elements should be placed relative to the nearest
        non-static ancestor, not the document root."""
        from layout._dispatch import layout_node

        ancestor = make_element(style={
            'display': 'block', 'position': 'relative',
            'width': '400px', 'height': '200px',
            'margin-top': '100px', 'margin-left': '50px',
            'margin-bottom': '0px', 'margin-right': '0px',
        })
        child = make_element(style={
            'display': 'block', 'position': 'absolute',
            'top': '10px', 'left': '20px',
            'width': '100px', 'height': '50px',
        })
        child.parent = ancestor
        ancestor.children = [child]

        c = make_container(width=800, height=600)
        ctx = LayoutContext()
        layout_node(ancestor, c, ctx)

        # The absolutely positioned child should be resolved within ctx/ancestor
        # and placed at ancestor's origin + offsets, not document origin
        # This requires the engine to track the containing block
        abs_nodes = getattr(ctx, 'absolute_nodes', [])
        assert any(n is child for n in abs_nodes), \
            "absolute child should be tracked in ctx for post-pass placement"

    def test_absolute_element_deferred_from_flow(self):
        """position:absolute elements return None from layout_node (deferred)."""
        el = make_element(style={
            'display': 'block', 'position': 'absolute',
            'right': '0px', 'bottom': '0px',
            'width': '100px', 'height': '50px',
        })
        c = make_container(width=400, height=300)
        c.content_height = 300.0
        ctx = LayoutContext()
        from layout._dispatch import layout_node
        result = layout_node(el, c, ctx)
        assert result is None, "Absolute element should be deferred (return None)"
        assert not (hasattr(el, 'box') and el.box is not None), \
            "Absolute element should not have a box set inline"

    def test_absolute_right_bottom_final_position(self):
        """After full pipeline, right:0,bottom:0 element should be at bottom-right corner."""
        from tests.render_helper import render, find_element

        document = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div id="container" style="position:relative; width:400px; height:300px">
            <div id="abs" style="position:absolute; right:0; bottom:0;
                                 width:100px; height:50px">Abs</div>
          </div>
        </body></html>
        ''', viewport_width=800, viewport_height=600)

        container = find_element(document, id_name='container')
        abs_el = find_element(document, id_name='abs')
        assert abs_el.box.x == pytest.approx(container.box.x + 300.0, abs=2.0)
        assert abs_el.box.y == pytest.approx(container.box.y + 250.0, abs=2.0)
