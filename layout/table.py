"""Table layout — CSS display:table."""
from __future__ import annotations
import logging
_logger = logging.getLogger(__name__)
import re
from layout.box import BoxModel, EdgeSizes
from layout.text import _parse_px, measure_text
from layout.context import LayoutEngine, LayoutContext

_TABLE_ROW_GROUPS = frozenset({
    'table-row-group', 'table-header-group', 'table-footer-group',
})


def _gs(node, prop: str, default: str = '') -> str:
    if hasattr(node, 'style') and node.style:
        return node.style.get(prop, default)
    return default


def _parse_edge(node, prop_prefix: str, cw: float = 0.0) -> EdgeSizes:
    e = EdgeSizes()
    for side in ('top', 'right', 'bottom', 'left'):
        if prop_prefix == 'border-width':
            val = _gs(node, f'border-{side}-width', '')
            if not val:
                val = _gs(node, f'border-width-{side}', '0px')
        else:
            val = _gs(node, f'{prop_prefix}-{side}', '0px')
        if val == 'auto':
            e.__dict__[side] = 0.0
        elif val and val.endswith('%') and cw > 0:
            try:
                e.__dict__[side] = float(val[:-1]) / 100.0 * cw
            except ValueError:
                e.__dict__[side] = 0.0
        else:
            e.__dict__[side] = _parse_px(val)
    return e


class TableLayout(LayoutEngine):
    """Layout engine for display:table."""

    def layout(self, node, container: BoxModel, ctx: LayoutContext) -> BoxModel:
        from html.dom import Element
        from layout.block import BlockLayout

        style = node.style or {}
        attrs = getattr(node, 'attributes', {})
        c_width = container.content_width
        margin = _parse_edge(node, 'margin', c_width)

        # Table width
        width_str = style.get('width', attrs.get('width', 'auto'))
        if width_str in ('auto', ''):
            table_w = max(0.0, c_width - margin.left - margin.right)
        elif width_str.endswith('%'):
            table_w = max(0.0, c_width * float(width_str[:-1]) / 100.0)
        else:
            table_w = max(0.0, _parse_px(width_str))

        # margin:auto centering
        ml = style.get('margin-left', '0px')
        mr = style.get('margin-right', '0px')
        if ml == 'auto' and mr == 'auto':
            table_x = container.x + max(0.0, c_width - table_w) / 2.0
        elif ml == 'auto':
            table_x = container.x + max(0.0, c_width - table_w - margin.right)
        else:
            table_x = container.x + margin.left

        table_y = container.y + container.content_height + margin.top

        # Spacing
        bs_raw = style.get('border-spacing', attrs.get('cellspacing', '2'))
        try:
            cell_spacing = _parse_px(bs_raw) if isinstance(bs_raw, str) and bs_raw.endswith('px') else float(bs_raw)
        except Exception:
            cell_spacing = 2.0

        cp_raw = attrs.get('cellpadding', '0')
        try:
            cell_padding = float(cp_raw)
        except Exception:
            cell_padding = 0.0

        # Collect all rows
        all_rows = []  # [(tr_node, [(td_node, colspan)])]
        for child in node.children:
            if not isinstance(child, Element):
                continue
            disp = _gs(child, 'display', '')
            tag = child.tag
            if tag == 'tr' or disp == 'table-row':
                all_rows.append((child, _extract_cells(child)))
            elif tag in ('thead', 'tbody', 'tfoot') or disp in _TABLE_ROW_GROUPS:
                for sub in child.children:
                    if isinstance(sub, Element) and (sub.tag == 'tr' or _gs(sub, 'display', '') == 'table-row'):
                        all_rows.append((sub, _extract_cells(sub)))

        # Column count
        n_cols = max((sum(cs for _, cs in cells) for _, cells in all_rows if cells), default=0)

        box = BoxModel()
        box.x = table_x
        box.y = table_y
        box.content_width = table_w
        box.margin = margin

        if n_cols == 0:
            # Empty table: still give it a height from any explicit TR heights
            total_h = cell_spacing
            for tr_node, _ in all_rows:
                h_str = _gs(tr_node, 'height', getattr(tr_node, 'attributes', {}).get('height', '0'))
                total_h += max(0.0, _parse_px(h_str)) + cell_spacing
            box.content_height = total_h
            return box

        col_widths = _compute_col_widths(all_rows, n_cols, table_w, cell_spacing)

        y_offset = cell_spacing
        for tr_node, cells in all_rows:
            tr_attrs = getattr(tr_node, 'attributes', {})
            tr_h_str = _gs(tr_node, 'height', tr_attrs.get('height', '0'))
            tr_min_h = _parse_px(tr_h_str) if tr_h_str and tr_h_str != '0' else 0.0

            row_y = table_y + y_offset
            max_cell_h = tr_min_h

            if not cells:
                # Spacer row with no cells — just reserve the height
                tr_box = BoxModel()
                tr_box.x = table_x; tr_box.y = row_y
                tr_box.content_width = table_w
                tr_box.content_height = tr_min_h
                tr_node.box = tr_box;                y_offset += tr_min_h + cell_spacing
                continue

            x_offset = cell_spacing
            col_idx = 0
            for td_node, colspan in cells:
                cell_w = sum(col_widths[col_idx:col_idx + colspan]) + cell_spacing * (colspan - 1)
                td_style = td_node.style or {}
                has_css_pad = any(
                    _parse_px(td_style.get(f'padding-{s}', '0')) > 0
                    for s in ('top', 'right', 'bottom', 'left')
                )

                cell_cont = BoxModel()
                cell_cont.x = table_x + x_offset
                cell_cont.y = row_y
                cell_cont.content_width = cell_w
                cell_cont.content_height = 0.0

                # Override td width to 'auto' so BlockLayout uses the
                # resolved column width instead of re-interpreting the
                # percentage against the (already-resolved) container.
                saved_width = td_style.get('width')
                td_style['width'] = 'auto'
                saved_padding = {}
                if not has_css_pad and cell_padding > 0.0:
                    for side in ('top', 'right', 'bottom', 'left'):
                        key = f'padding-{side}'
                        saved_padding[key] = td_style.get(key)
                        td_style[key] = f'{cell_padding}px'

                child_ctx = ctx.fork()  # cells form new BFC
                cell_box = BlockLayout().layout(td_node, cell_cont, child_ctx)

                # Restore original width style
                if saved_width is not None:
                    td_style['width'] = saved_width
                else:
                    td_style.pop('width', None)
                if saved_padding:
                    for key, value in saved_padding.items():
                        if value is None:
                            td_style.pop(key, None)
                        else:
                            td_style[key] = value
                td_node.box = cell_box
                valign = td_style.get('vertical-align', 'middle')
                total_cell_h = (
                    cell_box.content_height
                    + cell_box.padding.top + cell_box.padding.bottom
                    + cell_box.border.top + cell_box.border.bottom
                )
                max_cell_h = max(max_cell_h, total_cell_h)
                td_node._table_valign = valign
                x_offset += cell_w + cell_spacing
                col_idx += colspan

            tr_box = BoxModel()
            tr_box.x = table_x; tr_box.y = row_y
            tr_box.content_width = table_w; tr_box.content_height = max_cell_h
            tr_node.box = tr_box;
            x_offset = cell_spacing
            col_idx = 0
            for td_node, colspan in cells:
                cell_w = sum(col_widths[col_idx:col_idx + colspan]) + cell_spacing * (colspan - 1)
                cell_box = getattr(td_node, 'box', None)
                if cell_box is not None:
                    pl = cell_box.padding.left
                    pr = cell_box.padding.right
                    pt = cell_box.padding.top
                    pb = cell_box.padding.bottom
                    bt = cell_box.border.top
                    bb = cell_box.border.bottom
                    total_vert = pt + pb + bt + bb
                    extra_h = max(0.0, max_cell_h - (cell_box.content_height + total_vert))
                    # Stretch cell to fill row height
                    cell_box.content_height = max(0.0, max_cell_h - total_vert)
                    valign = getattr(td_node, '_table_valign', 'middle')
                    if valign == 'bottom':
                        dy = extra_h
                    elif valign in ('middle', 'center'):
                        dy = extra_h / 2.0
                    else:
                        dy = 0.0

                    # Horizontal alignment for block-level children only.
                    # Inline text is already positioned by LineBox.finalize()
                    # via text-align, so we must NOT shift line boxes here.
                    dx = 0.0
                    td_style = td_node.style or {}
                    align = td_style.get('text-align', 'left')
                    if align in ('center', 'right'):
                        block_w = _measure_block_children_width(td_node)
                        if block_w > 0:
                            inner_w = max(0.0, cell_box.content_width)
                            if align == 'right':
                                dx = max(0.0, inner_w - block_w)
                            else:
                                dx = max(0.0, (inner_w - block_w) / 2.0)

                    if dx or dy:
                        _shift_block_children(td_node, dx, dy)

                x_offset += cell_w + cell_spacing
                col_idx += colspan
            y_offset += max_cell_h + cell_spacing

        box.content_height = y_offset
        return box


def _extract_cells(tr_node) -> list:
    """Return [(td_node, colspan)] for a <tr>. Returns [] (not None) for empty rows."""
    from html.dom import Element
    cells = []
    for child in tr_node.children:
        if not isinstance(child, Element):
            continue
        disp = _gs(child, 'display', '')
        if child.tag not in ('td', 'th') and disp != 'table-cell':
            continue
        if _gs(child, 'display', 'table-cell') == 'none':
            continue
        try:
            colspan = max(1, int(getattr(child, 'attributes', {}).get('colspan', '1')))
        except (ValueError, AttributeError):
            colspan = 1
        cells.append((child, colspan))
    return cells


def _compute_col_widths(all_rows, n_cols: int, table_w: float, cell_spacing: float) -> list:
    available = table_w - cell_spacing * (n_cols + 1)
    col_widths = [None] * n_cols
    auto_min = [0.0] * n_cols
    nowrap_min = [0.0] * n_cols  # minimum width for nowrap cells
    for _tr, cells in all_rows:
        col_idx = 0
        for td_node, colspan in cells:
            td_style = td_node.style or {}
            if colspan == 1:
                # Track nowrap minimum width regardless of explicit width
                ws = td_style.get('white-space', '')
                if ws == 'nowrap' or 'nowrap' in getattr(td_node, 'attributes', {}):
                    nowrap_min[col_idx] = max(nowrap_min[col_idx],
                                              _measure_cell_nowrap_width(td_node))
                w_str = td_style.get('width', '') or getattr(td_node, 'attributes', {}).get('width', '')
                if w_str and w_str not in ('auto', ''):
                    try:
                        w = (available * float(w_str[:-1]) / 100.0 if w_str.endswith('%')
                             else _parse_px(w_str))
                        if col_widths[col_idx] is None:
                            col_widths[col_idx] = w
                    except Exception as _exc:
                        _logger.debug("Ignored: %s", _exc)
                else:
                    auto_min[col_idx] = max(auto_min[col_idx], _measure_cell_min_width(td_node))
            col_idx += colspan

    # Ensure nowrap columns are at least as wide as their content
    for i in range(n_cols):
        if col_widths[i] is not None and nowrap_min[i] > col_widths[i]:
            col_widths[i] = nowrap_min[i]

    fixed = sum(w for w in col_widths if w is not None)
    n_auto = sum(1 for w in col_widths if w is None)
    remaining = max(0.0, available - fixed)
    min_total = sum(auto_min[i] for i, w in enumerate(col_widths) if w is None)

    if n_auto == 0:
        return [max(0.0, w) for w in col_widths]

    if min_total <= 0:
        auto_w = remaining / n_auto
        return [w if w is not None else auto_w for w in col_widths]

    widths = []
    for i, w in enumerate(col_widths):
        if w is not None:
            widths.append(max(0.0, w))
            continue
        share = remaining * (auto_min[i] / min_total) if min_total > 0 else 0.0
        widths.append(max(auto_min[i], share))

    used = sum(widths)
    if used > available and used > 0:
        scale = available / used
        widths = [w * scale for w in widths]
    return widths


def _measure_cell_min_width(node) -> float:
    """Estimate a table cell's minimum intrinsic width from its contents."""
    from html.dom import Element, Text

    style = getattr(node, 'style', {}) or {}
    width_str = style.get('width', '') or getattr(node, 'attributes', {}).get('width', '')
    if width_str and width_str not in ('auto', ''):
        try:
            return float(_parse_px(width_str))
        except Exception as _exc:
            _logger.debug("Ignored: %s", _exc)
    if getattr(node, 'tag', '') == 'img':
        natural = getattr(node, 'natural_width', 0) or 0
        if natural > 0:
            return float(natural)

    width = 0.0
    for child in getattr(node, 'children', []):
        if isinstance(child, Text):
            for word in child.data.split():
                if not word:
                    continue
                try:
                    w, _ = measure_text(
                        word,
                        style.get('font-family', 'Arial'),
                        _parse_px(style.get('font-size', '16px')),
                        style.get('font-weight', 'normal'),
                        style.get('font-style', 'normal') in ('italic', 'oblique'),
                    )
                    width = max(width, w)
                except Exception:
                    width = max(width, len(word) * max(1.0, _parse_px(style.get('font-size', '16px')) * 0.6))
        elif isinstance(child, Element):
            width = max(width, _measure_cell_min_width(child))
    return width


def _measure_cell_nowrap_width(node) -> float:
    """Measure the total inline content width of a nowrap cell (all words on one line)."""
    from html.dom import Element, Text

    style = getattr(node, 'style', {}) or {}
    family = style.get('font-family', 'Arial')
    size_px = _parse_px(style.get('font-size', '16px'))
    weight = style.get('font-weight', 'normal')
    italic = style.get('font-style', 'normal') in ('italic', 'oblique')

    total = 0.0
    try:
        space_w, _ = measure_text(' ', family, size_px, weight, italic)
    except Exception:
        space_w = size_px * 0.3

    def _walk(n, inherited_style):
        nonlocal total
        if isinstance(n, Text):
            s = inherited_style
            fam = s.get('font-family', family)
            sz = _parse_px(s.get('font-size', f'{size_px}px'))
            wt = s.get('font-weight', weight)
            it = s.get('font-style', 'normal') in ('italic', 'oblique')
            for token in re.findall(r'\S+|\s+', n.data):
                if token.isspace():
                    total += space_w
                    continue
                try:
                    w, _ = measure_text(token, fam, sz, wt, it)
                    total += w
                except Exception:
                    total += len(token) * sz * 0.6
        elif isinstance(n, Element):
            s = n.style or inherited_style
            for c in n.children:
                _walk(c, s)

    for child in node.children:
        _walk(child, style)
    return total


def _measure_block_children_width(node) -> float:
    """Measure the max width of block-level children (not line boxes)."""
    from html.dom import Element
    max_w = 0.0
    for child in getattr(node, 'children', []):
        if not isinstance(child, Element):
            continue
        child_box = getattr(child, 'box', None)
        if child_box is not None:
            max_w = max(max_w, child_box.content_width)
    return max_w


def _shift_block_children(node, dx: float, dy: float) -> None:
    """Shift block-level children and line boxes vertically only.

    dx is applied only to block children (with box), not to line boxes
    which already handle text-align. dy applies to everything.
    """
    from html.dom import Element
    # Shift line boxes vertically only
    if hasattr(node, 'line_boxes') and dy:
        for lb in node.line_boxes:
            lb.y += dy
            for item in lb.items:
                item.y += dy
    # Shift block children both horizontally and vertically
    for child in getattr(node, 'children', []):
        if not isinstance(child, Element):
            continue
        if hasattr(child, 'box') and child.box is not None:
            child.box.x += dx
            child.box.y += dy
        if hasattr(child, 'line_boxes'):
            for lb in child.line_boxes:
                lb.x += dx
                lb.y += dy
                for item in lb.items:
                    item.x += dx
                    item.y += dy
        _shift_subtree(child, dx, dy)


def _shift_subtree(node, dx: float, dy: float) -> None:
    from html.dom import Element

    for child in getattr(node, 'children', []):
        if not isinstance(child, Element):
            continue
        if hasattr(child, 'box') and child.box is not None:
            child.box.x += dx
            child.box.y += dy
        if hasattr(child, 'line_boxes'):
            for lb in child.line_boxes:
                lb.x += dx
                lb.y += dy
                for item in lb.items:
                    item.x += dx
                    item.y += dy
        _shift_subtree(child, dx, dy)


def _shift_cell_contents(node, dx: float, dy: float) -> None:
    if hasattr(node, 'line_boxes'):
        for lb in node.line_boxes:
            lb.x += dx
            lb.y += dy
            for item in lb.items:
                item.x += dx
                item.y += dy
    _shift_subtree(node, dx, dy)


def _measure_cell_content_width(node) -> float:
    max_width = 0.0

    if hasattr(node, 'line_boxes'):
        for lb in node.line_boxes:
            max_width = max(max_width, sum(item.width for item in lb.items))

    for child in getattr(node, 'children', []):
        child_box = getattr(child, 'box', None)
        if child_box is not None:
            max_width = max(max_width, child_box.content_width)
        max_width = max(max_width, _measure_cell_content_width(child))
    return max_width


# Backward-compat shim
def layout_table(node, container_box: BoxModel, viewport_width: int = 980) -> BoxModel:
    ctx = LayoutContext(viewport_width)
    box = TableLayout().layout(node, container_box, ctx)
    if box is not None:
        node.box = box
    return box
