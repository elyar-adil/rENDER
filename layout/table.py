"""Table layout — CSS display:table."""
from __future__ import annotations
from layout.box import BoxModel, EdgeSizes
from layout.text import _parse_px
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
            box.update_legacy()
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
                tr_node.box = tr_box; tr_box.update_legacy()
                y_offset += tr_min_h + cell_spacing
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
                cell_box.x = table_x + x_offset + pad
                cell_box.y = row_y + pad
                cell_box.content_width = inner_w
                cell_box.update_legacy()

                max_cell_h = max(max_cell_h, cell_box.content_height + 2 * pad)
                x_offset += cell_w + cell_spacing
                col_idx += colspan

            tr_box = BoxModel()
            tr_box.x = table_x; tr_box.y = row_y
            tr_box.content_width = table_w; tr_box.content_height = max_cell_h
            tr_node.box = tr_box; tr_box.update_legacy()
            y_offset += max_cell_h + cell_spacing

        box.content_height = y_offset
        box.update_legacy()
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
    for _tr, cells in all_rows:
        col_idx = 0
        for td_node, colspan in cells:
            if colspan == 1:
                td_style = td_node.style or {}
                w_str = td_style.get('width', '') or getattr(td_node, 'attributes', {}).get('width', '')
                if w_str and w_str not in ('auto', ''):
                    try:
                        w = (available * float(w_str[:-1]) / 100.0 if w_str.endswith('%')
                             else _parse_px(w_str))
                        if col_widths[col_idx] is None:
                            col_widths[col_idx] = w
                    except Exception:
                        pass
            col_idx += colspan

    fixed = sum(w for w in col_widths if w is not None)
    n_auto = sum(1 for w in col_widths if w is None)
    auto_w = max(0.0, (available - fixed) / n_auto) if n_auto > 0 else 0.0
    return [w if w is not None else auto_w for w in col_widths]


# Backward-compat shim
def layout_table(node, container_box: BoxModel, viewport_width: int = 980) -> BoxModel:
    ctx = LayoutContext(viewport_width)
    box = TableLayout().layout(node, container_box, ctx)
    if box is not None:
        node.box = box
        box.update_legacy()
    return box
