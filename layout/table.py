"""Table layout — implements display:table/table-row/table-cell."""
from layout.box import BoxModel, EdgeSizes
from layout.text import _parse_px


_TABLE_ROW_GROUPS = frozenset({
    'table-row-group', 'table-header-group', 'table-footer-group',
})


def _get_style(node, prop: str, default: str = '') -> str:
    if hasattr(node, 'style') and node.style:
        return node.style.get(prop, default)
    return default


def _parse_edge(node, prop_prefix: str, container_width: float = 0.0) -> EdgeSizes:
    edges = EdgeSizes()
    for side in ('top', 'right', 'bottom', 'left'):
        val = _get_style(node, f'{prop_prefix}-{side}', '0px')
        if val == 'auto':
            edges.__dict__[side] = 0.0
        elif val and val.endswith('%') and container_width > 0:
            try:
                edges.__dict__[side] = float(val[:-1]) / 100.0 * container_width
            except ValueError:
                edges.__dict__[side] = 0.0
        else:
            edges.__dict__[side] = _parse_px(val)
    return edges


def layout_table(node, container_box: BoxModel, viewport_width: int = 980) -> BoxModel:
    """Layout a display:table element. Returns the table's BoxModel."""
    from html.dom import Element
    from layout.block import layout_block

    style = node.style or {}
    attrs = node.attributes if hasattr(node, 'attributes') else {}

    c_width = container_box.content_width
    margin = _parse_edge(node, 'margin', c_width)

    # Table width
    width_str = style.get('width', attrs.get('width', 'auto'))
    if width_str == 'auto' or not width_str:
        table_content_width = max(0.0, c_width - margin.left - margin.right)
    elif width_str.endswith('%'):
        table_content_width = max(0.0, c_width * float(width_str[:-1]) / 100.0)
    else:
        table_content_width = max(0.0, _parse_px(width_str))

    # Handle margin:auto centering
    margin_left_str = style.get('margin-left', '0px')
    margin_right_str = style.get('margin-right', '0px')
    if margin_left_str == 'auto' and margin_right_str == 'auto':
        remaining = max(0.0, c_width - table_content_width)
        table_x = container_box.x + remaining / 2.0
    elif margin_left_str == 'auto':
        remaining = max(0.0, c_width - table_content_width - margin.right)
        table_x = container_box.x + remaining
    else:
        table_x = container_box.x + margin.left

    table_y = container_box.y + container_box.content_height + margin.top

    # Cell spacing/padding
    bs_str = style.get('border-spacing', attrs.get('cellspacing', '2'))
    try:
        cell_spacing = _parse_px(bs_str) if bs_str.endswith('px') else float(bs_str)
    except Exception:
        cell_spacing = 2.0

    cp_str = attrs.get('cellpadding', '0')
    try:
        cell_padding = float(cp_str)
    except Exception:
        cell_padding = 0.0

    # Collect rows from thead/tbody/tfoot/direct tr children
    all_rows = []  # list of (tr_node, [(td_node, colspan)])
    for child in node.children:
        if not isinstance(child, Element):
            continue
        disp = _get_style(child, 'display', '')
        tag = child.tag
        if tag == 'tr' or disp == 'table-row':
            row = _extract_cells(child)
            if row is not None:
                all_rows.append((child, row))
        elif tag in ('thead', 'tbody', 'tfoot') or disp in _TABLE_ROW_GROUPS:
            for sub in child.children:
                if not isinstance(sub, Element):
                    continue
                if sub.tag == 'tr' or _get_style(sub, 'display', '') == 'table-row':
                    row = _extract_cells(sub)
                    if row is not None:
                        all_rows.append((sub, row))

    # Determine column count
    n_cols = 0
    for _tr, cells in all_rows:
        n_cols = max(n_cols, sum(cs for _, cs in cells))

    if n_cols == 0:
        box = BoxModel()
        box.x = table_x
        box.y = table_y
        box.content_width = table_content_width
        box.content_height = 0.0
        box.margin = margin
        box.update_legacy()
        return box

    # Column widths
    col_widths = _compute_col_widths(all_rows, n_cols, table_content_width, cell_spacing)

    # Layout rows
    box = BoxModel()
    box.x = table_x
    box.y = table_y
    box.content_width = table_content_width
    box.margin = margin

    y_offset = cell_spacing

    for tr_node, cells in all_rows:
        # Minimum row height from TR style/attr
        tr_h_str = _get_style(tr_node, 'height', tr_node.attributes.get('height', '0'))
        tr_min_h = _parse_px(tr_h_str) if tr_h_str and tr_h_str != '0' else 0.0

        row_y = table_y + y_offset
        max_cell_h = tr_min_h
        x_offset = cell_spacing
        col_idx = 0

        for td_node, colspan in cells:
            # Cell width = spanned columns + spacings between them
            cell_w = sum(col_widths[col_idx:col_idx + colspan]) + cell_spacing * (colspan - 1)

            # Cell's own padding (CSS padding > cellpadding attr)
            td_style = td_node.style or {}
            has_css_padding = any(
                td_style.get(f'padding-{s}', '') not in ('', '0px', '0')
                for s in ('top', 'right', 'bottom', 'left')
            ) or td_style.get('padding', '') not in ('', '0px', '0')
            pad = 0.0 if has_css_padding else cell_padding

            inner_w = max(0.0, cell_w - 2 * pad)

            cell_container = BoxModel()
            cell_container.x = table_x + x_offset + pad
            cell_container.y = row_y + pad
            cell_container.content_width = inner_w
            cell_container.content_height = 0.0

            cell_box = layout_block(td_node, cell_container, viewport_width=viewport_width)
            td_node.box = cell_box
            cell_box.x = table_x + x_offset + pad
            cell_box.y = row_y + pad
            cell_box.content_width = inner_w
            cell_box.update_legacy()

            max_cell_h = max(max_cell_h, cell_box.content_height + 2 * pad)
            x_offset += cell_w + cell_spacing
            col_idx += colspan

        # Set row box
        tr_box = BoxModel()
        tr_box.x = table_x
        tr_box.y = row_y
        tr_box.content_width = table_content_width
        tr_box.content_height = max_cell_h
        tr_node.box = tr_box
        tr_box.update_legacy()

        y_offset += max_cell_h + cell_spacing

    box.content_height = y_offset
    box.update_legacy()
    return box


def _extract_cells(tr_node) -> list:
    """Return [(td_node, colspan)] for a <tr>."""
    from html.dom import Element
    cells = []
    for child in tr_node.children:
        if not isinstance(child, Element):
            continue
        disp = _get_style(child, 'display', '')
        if child.tag not in ('td', 'th') and disp != 'table-cell':
            continue
        if _get_style(child, 'display', 'table-cell') == 'none':
            continue
        try:
            colspan = max(1, int(child.attributes.get('colspan', '1')))
        except (ValueError, AttributeError):
            colspan = 1
        cells.append((child, colspan))
    return cells if cells else None


def _compute_col_widths(all_rows, n_cols: int, table_width: float,
                        cell_spacing: float) -> list:
    """Compute column widths. Explicit > percentage > auto."""
    available = table_width - cell_spacing * (n_cols + 1)

    col_widths = [None] * n_cols  # None = auto

    # First pass: assign explicit/percentage widths
    for _tr, cells in all_rows:
        col_idx = 0
        for td_node, colspan in cells:
            if colspan == 1:
                td_style = td_node.style or {}
                w_str = td_style.get('width', '') or td_node.attributes.get('width', '')
                if w_str and w_str not in ('auto', ''):
                    if w_str.endswith('%'):
                        w = available * float(w_str[:-1]) / 100.0
                    else:
                        w = _parse_px(w_str)
                    if col_widths[col_idx] is None:
                        col_widths[col_idx] = w
            col_idx += colspan

    # Auto columns get equal share
    fixed = sum(w for w in col_widths if w is not None)
    n_auto = sum(1 for w in col_widths if w is None)
    auto_w = max(0.0, (available - fixed) / n_auto) if n_auto > 0 else 0.0
    return [w if w is not None else auto_w for w in col_widths]
