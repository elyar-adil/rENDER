"""Table layout — CSS display:table."""
from __future__ import annotations
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
                    td_style.get(f'padding-{s}', '') not in ('', '0px', '0')
                    for s in ('top', 'right', 'bottom', 'left')
                ) or td_style.get('padding', '') not in ('', '0px', '0')
                pad = 0.0 if has_css_pad else cell_padding
                inner_w = max(0.0, cell_w - 2 * pad)

                cell_cont = BoxModel()
                cell_cont.x = table_x + x_offset + pad
                cell_cont.y = row_y + pad
                cell_cont.content_width = inner_w
                cell_cont.content_height = 0.0

                child_ctx = ctx.fork()  # cells form new BFC
                cell_box = BlockLayout().layout(td_node, cell_cont, child_ctx)
                td_node.box = cell_box
                valign = td_style.get('vertical-align', 'middle')
                align = td_style.get('text-align', 'left')
                cell_box.x = table_x + x_offset + pad
                cell_box.y = row_y + pad
                cell_box.content_width = inner_w

                max_cell_h = max(max_cell_h, cell_box.content_height + 2 * pad)
                td_node._table_align = align
                td_node._table_valign = valign
                td_node._table_pad = pad
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
                pad = getattr(td_node, '_table_pad', 0.0)
                cell_box = getattr(td_node, 'box', None)
                if cell_box is not None:
                    extra_h = max(0.0, max_cell_h - (cell_box.content_height + 2 * pad))
                    valign = getattr(td_node, '_table_valign', 'middle')
                    if valign == 'bottom':
                        dy = extra_h
                    elif valign in ('middle', 'center'):
                        dy = extra_h / 2.0
                    else:
                        dy = 0.0

                    dx = 0.0
                    align = getattr(td_node, '_table_align', 'left')
                    content_w = _measure_cell_content_width(td_node)
                    if content_w <= 0:
                        content_w = cell_box.content_width
                    if align == 'right':
                        dx = max(0.0, cell_w - 2 * pad - content_w)
                    elif align == 'center':
                        dx = max(0.0, (cell_w - 2 * pad - content_w) / 2.0)

                    if dx or dy:
                        _shift_cell_contents(td_node, dx, dy)

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
    for _tr, cells in all_rows:
        col_idx = 0
        for td_node, colspan in cells:
            td_style = td_node.style or {}
            if colspan == 1:
                w_str = td_style.get('width', '') or getattr(td_node, 'attributes', {}).get('width', '')
                if w_str and w_str not in ('auto', ''):
                    try:
                        w = (available * float(w_str[:-1]) / 100.0 if w_str.endswith('%')
                             else _parse_px(w_str))
                        if col_widths[col_idx] is None:
                            col_widths[col_idx] = w
                    except Exception:
                        pass
                else:
                    auto_min[col_idx] = max(auto_min[col_idx], _measure_cell_min_width(td_node))
            col_idx += colspan

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
        except Exception:
            pass
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
