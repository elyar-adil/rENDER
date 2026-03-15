"""CSS Grid layout (simplified — supports fixed/fr/auto column tracks)."""
from __future__ import annotations
from layout.box import BoxModel, EdgeSizes
from layout.text import _parse_px
from layout.context import LayoutEngine, LayoutContext


def _get_style(node, prop: str, default: str = '') -> str:
    if hasattr(node, 'style') and node.style:
        return node.style.get(prop, default)
    return default


def _parse_edge(node, prefix: str) -> EdgeSizes:
    from layout.block import _parse_edge as _pe
    return _pe(node, prefix)


def _parse_track_list(template: str, available: float) -> list:
    """Parse grid-template-columns/rows value into pixel sizes.

    Handles: px values, fr units, auto, repeat(N, size), percentages.
    Returns list of track pixel sizes.
    """
    if not template or template in ('none', ''):
        return []

    # Expand repeat(N, size) or repeat(N, size size ...)
    import re
    def _expand_repeat(m):
        count = int(m.group(1))
        sizes = m.group(2).strip()
        return ' '.join([sizes] * count)

    template = re.sub(r'repeat\(\s*(\d+)\s*,\s*([^)]+)\)', _expand_repeat, template)

    # Tokenize by spaces (but keep things like minmax(...) together)
    tokens = []
    depth = 0
    cur = []
    for ch in template:
        if ch == '(':
            depth += 1
            cur.append(ch)
        elif ch == ')':
            depth -= 1
            cur.append(ch)
        elif ch in (' ', '\t') and depth == 0:
            if cur:
                tokens.append(''.join(cur))
                cur = []
        else:
            cur.append(ch)
    if cur:
        tokens.append(''.join(cur))

    # First pass: measure fixed + % tracks, count fr units
    sizes = []
    fr_total = 0.0
    fixed_total = 0.0
    for tok in tokens:
        if tok.endswith('fr'):
            try:
                fr = float(tok[:-2])
            except ValueError:
                fr = 1.0
            sizes.append(('fr', fr))
            fr_total += fr
        elif tok == 'auto':
            sizes.append(('auto', 0.0))
        elif tok.startswith('minmax('):
            # simplify: use max value
            inner = tok[7:-1]
            parts = inner.split(',', 1)
            val = parts[1].strip() if len(parts) > 1 else parts[0].strip()
            try:
                px = _parse_px(val) if not val.endswith('fr') else 0.0
            except Exception:
                px = 0.0
            sizes.append(('px', px))
            fixed_total += px
        elif tok.endswith('%'):
            try:
                px = available * float(tok[:-1]) / 100
            except ValueError:
                px = 0.0
            sizes.append(('px', px))
            fixed_total += px
        else:
            try:
                px = _parse_px(tok)
            except Exception:
                px = 0.0
            sizes.append(('px', px))
            fixed_total += px

    # Second pass: resolve fr and auto
    remaining = max(0.0, available - fixed_total)
    auto_count = sum(1 for kind, _ in sizes if kind == 'auto')
    auto_px = (remaining / (fr_total + auto_count)) if (fr_total + auto_count) > 0 else 0.0

    result = []
    for kind, val in sizes:
        if kind == 'px':
            result.append(val)
        elif kind == 'fr':
            result.append(remaining * val / fr_total if fr_total > 0 else 0.0)
        else:  # auto
            result.append(auto_px)

    return result


class GridLayout(LayoutEngine):
    """Layout engine for display:grid."""

    def layout(self, node, container: BoxModel, ctx: LayoutContext) -> BoxModel:
        from html.dom import Element
        from layout.block import layout_block, _parse_edge, BlockLayout
        from layout.flex import FlexLayout

        box = BoxModel()
        margin = _parse_edge(node, 'margin')
        padding = _parse_edge(node, 'padding')
        border_w = _parse_edge(node, 'border-width')

        width_str = _get_style(node, 'width', 'auto')
        c_width = container.content_width

        if width_str == 'auto' or not width_str:
            content_width = max(0.0, c_width - margin.left - margin.right
                                - padding.left - padding.right
                                - border_w.left - border_w.right)
        elif width_str.endswith('%'):
            pct = float(width_str[:-1]) / 100
            content_width = max(0.0, c_width * pct - margin.left - margin.right
                                - padding.left - padding.right
                                - border_w.left - border_w.right)
        else:
            content_width = max(0.0, _parse_px(width_str))

        box.x = container.x + margin.left + border_w.left + padding.left
        box.y = container.y + container.content_height + margin.top
        box.content_width = content_width
        box.margin = margin
        box.padding = padding
        box.border = border_w

        # Parse gap
        col_gap_str = _get_style(node, 'column-gap', _get_style(node, 'gap', '0'))
        row_gap_str = _get_style(node, 'row-gap', _get_style(node, 'gap', '0'))
        try:
            col_gap = _parse_px(col_gap_str)
        except Exception:
            col_gap = 0.0
        try:
            row_gap = _parse_px(row_gap_str)
        except Exception:
            row_gap = 0.0

        # Parse column/row template
        col_template = _get_style(node, 'grid-template-columns', '')
        col_tracks = _parse_track_list(col_template, content_width) if col_template else []

        # Collect grid children (skip display:none)
        children = [c for c in node.children
                    if isinstance(c, Element) and _get_style(c, 'display') != 'none']

        if not children:
            box.content_height = 0.0
            box.update_legacy()
            return box

        # If no explicit column template, use a single column (block-like layout)
        if not col_tracks:
            col_tracks = [content_width]

        n_cols = len(col_tracks)

        # Layout each child into its grid cell
        # Simple auto-placement: fill row by row, left-to-right
        cur_row = 0
        cur_col = 0
        row_heights = []  # list of row heights
        cell_boxes = []   # (child, box, row, col)

        for child in children:
            # compute column x positions
            col_x_positions = []
            cx = box.x
            for i, tw in enumerate(col_tracks):
                col_x_positions.append(cx)
                cx += tw + (col_gap if i < n_cols - 1 else 0)

            # Build child container at current grid cell
            child_container = BoxModel()
            child_container.x = col_x_positions[cur_col]
            child_container.y = box.y  # will adjust after row height known
            child_container.content_width = col_tracks[cur_col]
            child_container.content_height = 0.0

            child_display = _get_style(child, 'display', 'block')
            if child_display == 'flex':
                cb = FlexLayout().layout(child, child_container, ctx)
            else:
                cb = BlockLayout().layout(child, child_container, ctx)

            cell_boxes.append((child, cb, cur_row, cur_col))

            # Advance placement
            cur_col += 1
            if cur_col >= n_cols:
                cur_col = 0
                cur_row += 1

        # Compute row heights
        n_rows = (len(children) + n_cols - 1) // n_cols
        row_heights = [0.0] * n_rows
        for child, cb, row, col in cell_boxes:
            row_heights[row] = max(row_heights[row],
                                   cb.content_height + cb.padding.top + cb.padding.bottom
                                   + cb.border.top + cb.border.bottom)

        # Check for explicit row template
        row_template = _get_style(node, 'grid-template-rows', '')
        if row_template:
            total_h = sum(row_heights)
            explicit_rows = _parse_track_list(row_template, total_h)
            for i, h in enumerate(explicit_rows):
                if i < len(row_heights):
                    row_heights[i] = max(row_heights[i], h)

        # Compute row y positions
        row_y = [box.y]
        for i in range(1, n_rows):
            row_y.append(row_y[i - 1] + row_heights[i - 1] + row_gap)

        # Apply positions
        col_x_positions = []
        cx = box.x
        for i, tw in enumerate(col_tracks):
            col_x_positions.append(cx)
            cx += tw + (col_gap if i < n_cols - 1 else 0)

        for child, cb, row, col in cell_boxes:
            align = _get_style(node, 'align-items', 'stretch')
            justify = _get_style(node, 'justify-items', 'stretch')

            target_x = col_x_positions[col]
            target_y = row_y[row]
            row_h = row_heights[row]

            if align == 'center':
                target_y += (row_h - cb.content_height) / 2
            elif align == 'end' or align == 'flex-end':
                target_y += row_h - cb.content_height - cb.padding.bottom - cb.border.bottom

            cb.x = target_x + cb.margin.left + cb.border.left + cb.padding.left
            cb.y = target_y + cb.margin.top + cb.border.top + cb.padding.top
            cb.update_legacy()
            child.box = cb

        # Total height
        if row_heights:
            total_h = sum(row_heights) + row_gap * max(0, n_rows - 1)
        else:
            total_h = 0.0

        height_str = _get_style(node, 'height', 'auto')
        if height_str and height_str not in ('auto', ''):
            try:
                total_h = _parse_px(height_str)
            except Exception:
                pass

        box.content_height = max(total_h, 0.0)
        box.update_legacy()
        return box


# Backward-compat shim
def layout_grid(node, container_box: BoxModel) -> BoxModel:
    """Backward-compatible wrapper around GridLayout."""
    ctx = LayoutContext()
    return GridLayout().layout(node, container_box, ctx)
