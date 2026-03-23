"""Flexbox layout (simplified)."""
from __future__ import annotations
from layout.box import BoxModel, EdgeSizes
from layout.text import _parse_px
from layout.context import LayoutEngine, LayoutContext


def _get_style(node, prop: str, default='') -> str:
    if hasattr(node, 'style') and node.style:
        return node.style.get(prop, default)
    return default


def _parse_gap(node, direction: str) -> float:
    """Parse gap / row-gap / column-gap for the given axis direction."""
    gap_val = _get_style(node, 'gap', '')
    if gap_val:
        # gap shorthand: "row-gap column-gap" or single value for both
        parts = gap_val.split()
        if direction == 'column' and len(parts) >= 2:
            return _parse_px(parts[1])
        return _parse_px(parts[0])

    if direction == 'row':
        return _parse_px(_get_style(node, 'row-gap', '0'))
    else:
        return _parse_px(_get_style(node, 'column-gap', '0'))


class FlexLayout(LayoutEngine):
    """Layout engine for display:flex."""

    def layout(self, node, container: BoxModel, ctx: LayoutContext) -> BoxModel:
        from html.dom import Element
        from layout.block import _parse_edge, BlockLayout

        box = BoxModel()
        margin = _parse_edge(node, 'margin')
        padding = _parse_edge(node, 'padding')
        border_w = _parse_edge(node, 'border-width')

        width_str = _get_style(node, 'width', 'auto')
        c_width = container.content_width

        if width_str == 'auto' or width_str == '':
            content_width = max(0.0, c_width - margin.left - margin.right - padding.left - padding.right)
        elif width_str.endswith('%'):
            pct = float(width_str[:-1]) / 100
            content_width = c_width * pct
        else:
            content_width = _parse_px(width_str)

        box.x = container.x + margin.left + border_w.left + padding.left
        box.y = container.y + container.content_height + margin.top
        box.content_width = content_width
        box.margin = margin
        box.padding = padding
        box.border = border_w

        direction = _get_style(node, 'flex-direction', 'row')
        wrap = _get_style(node, 'flex-wrap', 'nowrap')
        justify = _get_style(node, 'justify-content', 'flex-start')
        align = _get_style(node, 'align-items', 'stretch')

        flex_children = [c for c in node.children if isinstance(c, Element) and _get_style(c, 'display') != 'none']

        # Sort by CSS order property (default 0); stable sort preserves DOM order for equal values
        flex_children.sort(key=lambda c: int(_get_style(c, 'order', '0') or '0'))

        if not flex_children:
            box.content_height = 0.0
            return box

        # Measure children — use a child context derived from flex ctx
        child_boxes = []
        for child in flex_children:
            child_container = BoxModel()
            child_container.x = box.x
            child_container.y = box.y
            child_container.content_width = box.content_width
            child_container.content_height = 0.0
            cb = BlockLayout().layout(child, child_container, ctx)
            child_boxes.append(cb)

        if direction in ('row', 'row-reverse'):
            # Column gap (space between items in a row)
            col_gap = _parse_gap(node, 'column')

            # --- flex-grow / flex-shrink distribution ---
            total_items_width = sum(cb.content_width + cb.margin.left + cb.margin.right for cb in child_boxes)
            n = len(child_boxes)
            gaps_total = col_gap * max(0, n - 1)
            remaining = content_width - total_items_width - gaps_total

            flex_grows = []
            flex_shrinks = []
            for child in flex_children:
                fg = float(_get_style(child, 'flex-grow', '0') or '0')
                fs = float(_get_style(child, 'flex-shrink', '1') or '1')
                flex_grows.append(fg)
                flex_shrinks.append(fs)

            total_grow = sum(flex_grows)
            total_weighted_shrink = sum(
                flex_shrinks[i] * (child_boxes[i].content_width + child_boxes[i].margin.left + child_boxes[i].margin.right)
                for i in range(n)
            )

            if remaining > 0 and total_grow > 0:
                # Distribute remaining space proportionally to flex-grow
                for i, cb in enumerate(child_boxes):
                    if flex_grows[i] > 0:
                        extra = remaining * (flex_grows[i] / total_grow)
                        child = flex_children[i]
                        new_content_w = cb.content_width + extra
                        # Pass the flex size via container width instead of mutating style
                        child_container = BoxModel()
                        child_container.x = box.x
                        child_container.y = box.y
                        child_container.content_width = new_content_w
                        child_container.content_height = 0.0
                        new_cb = BlockLayout().layout(child, child_container, ctx)
                        # Ensure width matches what flex allocated
                        new_cb.content_width = new_content_w
                        child_boxes[i] = new_cb

            elif remaining < 0 and total_weighted_shrink > 0:
                # Shrink items proportionally
                excess = -remaining  # positive
                for i, cb in enumerate(child_boxes):
                    if flex_shrinks[i] > 0:
                        item_w = cb.content_width + cb.margin.left + cb.margin.right
                        shrink_ratio = (flex_shrinks[i] * item_w) / total_weighted_shrink
                        reduction = excess * shrink_ratio
                        new_content_w = max(0.0, cb.content_width - reduction)
                        child = flex_children[i]
                        child_container = BoxModel()
                        child_container.x = box.x
                        child_container.y = box.y
                        child_container.content_width = new_content_w
                        child_container.content_height = 0.0
                        new_cb = BlockLayout().layout(child, child_container, ctx)
                        new_cb.content_width = new_content_w
                        child_boxes[i] = new_cb

            # --- flex-wrap: wrap ---
            if wrap == 'wrap':
                lines = _wrap_into_lines(flex_children, child_boxes, content_width, col_gap)
            else:
                lines = [(flex_children, child_boxes)]

            # Layout each line
            row_gap = _parse_gap(node, 'row')
            all_line_heights = []
            for line_children, line_boxes in lines:
                lh = max((cb.content_height + cb.margin.top + cb.margin.bottom for cb in line_boxes), default=0.0)
                all_line_heights.append(lh)

            y_cursor = box.y
            for line_idx, (line_children, line_boxes) in enumerate(lines):
                line_height = all_line_heights[line_idx]
                total_line_width = sum(cb.content_width + cb.margin.left + cb.margin.right for cb in line_boxes)
                n_line = len(line_boxes)
                gaps = col_gap * max(0, n_line - 1)
                slack = content_width - total_line_width - gaps

                x_positions = _compute_x_positions(justify, line_boxes, slack, col_gap, box.x)

                if direction == 'row-reverse':
                    x_positions = list(reversed(x_positions))

                for i, (child, cb) in enumerate(zip(line_children, line_boxes)):
                    cb.x = x_positions[i]
                    # align-self overrides align-items for this child
                    child_align = _get_style(child, 'align-self', 'auto')
                    effective_align = align if child_align in ('auto', '') else child_align
                    if effective_align == 'center':
                        cb.y = y_cursor + (line_height - cb.content_height) / 2
                    elif effective_align == 'flex-end':
                        cb.y = y_cursor + line_height - cb.content_height - cb.margin.bottom
                    elif effective_align == 'stretch':
                        cb.content_height = line_height
                        cb.height = line_height
                        cb.y = y_cursor + cb.margin.top
                    else:  # flex-start
                        cb.y = y_cursor + cb.margin.top
                    child.box = cb

                y_cursor += line_height
                if line_idx < len(lines) - 1:
                    y_cursor += row_gap

            box.content_height = y_cursor - box.y

        else:
            # Column layout
            row_gap = _parse_gap(node, 'row')
            y = box.y
            max_width = 0.0
            for idx, (child, cb) in enumerate(zip(flex_children, child_boxes)):
                cb.x = box.x + cb.margin.left
                cb.y = y + cb.margin.top
                y += cb.content_height + cb.margin.top + cb.margin.bottom
                if idx < len(flex_children) - 1:
                    y += row_gap
                max_width = max(max_width, cb.content_width)
                child.box = cb
            box.content_height = y - box.y

        return box


def _compute_x_positions(justify: str, child_boxes: list, slack: float, gap: float, base_x: float) -> list:
    """Compute x positions for a single flex line."""
    n = len(child_boxes)
    if justify in ('flex-start', 'start'):
        x_positions = []
        x = base_x
        for i, cb in enumerate(child_boxes):
            x_positions.append(x + cb.margin.left)
            x += cb.content_width + cb.margin.left + cb.margin.right + gap
    elif justify in ('flex-end', 'end'):
        x_positions = []
        x = base_x + slack
        for cb in child_boxes:
            x_positions.append(x + cb.margin.left)
            x += cb.content_width + cb.margin.left + cb.margin.right + gap
    elif justify == 'center':
        x_positions = []
        x = base_x + slack / 2
        for cb in child_boxes:
            x_positions.append(x + cb.margin.left)
            x += cb.content_width + cb.margin.left + cb.margin.right + gap
    elif justify == 'space-between':
        if n == 1:
            x_positions = [base_x]
        else:
            between_gap = (slack + gap * (n - 1)) / (n - 1) if n > 1 else 0
            x_positions = []
            x = base_x
            for cb in child_boxes:
                x_positions.append(x + cb.margin.left)
                x += cb.content_width + cb.margin.left + cb.margin.right + between_gap
    elif justify == 'space-around':
        around_gap = (slack + gap * (n - 1)) / n if n > 0 else 0
        x_positions = []
        x = base_x + around_gap / 2
        for cb in child_boxes:
            x_positions.append(x + cb.margin.left)
            x += cb.content_width + cb.margin.left + cb.margin.right + around_gap
    else:
        x_positions = []
        x = base_x
        for cb in child_boxes:
            x_positions.append(x + cb.margin.left)
            x += cb.content_width + cb.margin.left + cb.margin.right + gap
    return x_positions


def _wrap_into_lines(flex_children, child_boxes, content_width: float, gap: float) -> list:
    """Break flex children into wrap lines."""
    lines = []
    current_children = []
    current_boxes = []
    current_width = 0.0

    for child, cb in zip(flex_children, child_boxes):
        item_w = cb.content_width + cb.margin.left + cb.margin.right
        if current_children:
            needed = current_width + gap + item_w
        else:
            needed = item_w

        if current_children and needed > content_width:
            lines.append((current_children, current_boxes))
            current_children = [child]
            current_boxes = [cb]
            current_width = item_w
        else:
            current_children.append(child)
            current_boxes.append(cb)
            current_width = needed

    if current_children:
        lines.append((current_children, current_boxes))

    return lines


# Backward-compat shim
def layout_flex(node, container_box: BoxModel) -> BoxModel:
    """Backward-compatible wrapper around FlexLayout."""
    from layout.context import LayoutContext
    ctx = LayoutContext()
    return FlexLayout().layout(node, container_box, ctx)
