"""Flexbox layout (simplified)."""
import logging
_logger = logging.getLogger(__name__)
from layout.box import BoxModel, EdgeSizes, get_node_style as _get_style
from layout.text import _parse_px
from layout.context import LayoutEngine, LayoutContext


def _parse_gap(node, direction: str) -> float:
    """Parse gap / row-gap / column-gap for the given axis direction."""
    # Check the specific longhand first (it gets set by shorthand expansion)
    if direction == 'row':
        val = _get_style(node, 'row-gap', '0')
    else:
        val = _get_style(node, 'column-gap', '0')
    if val and val not in ('0', 'normal'):
        return _parse_px(val)

    # Fall back to gap shorthand
    gap_val = _get_style(node, 'gap', '')
    if gap_val and gap_val not in ('0', 'normal'):
        parts = gap_val.split()
        if direction == 'column' and len(parts) >= 2:
            return _parse_px(parts[1])
        return _parse_px(parts[0])

    return 0.0


def _get_flex_basis(child, container_width: float) -> float | None:
    """Return the flex-basis in px, or None for 'auto'."""
    basis = _get_style(child, 'flex-basis', 'auto')
    if basis in ('auto', ''):
        return None  # use width or content size
    if basis == '0' or basis == '0px':
        return 0.0
    if basis.endswith('%'):
        return container_width * float(basis[:-1]) / 100.0
    try:
        return _parse_px(basis)
    except Exception:
        return None


def _initial_main_size(child, child_box: BoxModel, container_width: float,
                       direction: str) -> float:
    """Compute the initial main-axis size for a flex item.

    This respects flex-basis, explicit width/height, and falls back to
    content size (min-content for auto-width flex items).
    """
    basis = _get_flex_basis(child, container_width)
    if basis is not None:
        return basis

    # flex-basis: auto → use width/height if specified, else content-based
    if direction in ('row', 'row-reverse'):
        width_str = _get_style(child, 'width', 'auto')
        if width_str not in ('auto', ''):
            if width_str.endswith('%'):
                return container_width * float(width_str[:-1]) / 100.0
            try:
                return _parse_px(width_str)
            except Exception as _exc:
                _logger.debug("Ignored: %s", _exc)
        # Auto width flex item: use intrinsic (min-content) width, not container
        from layout.inline import _measure_inline_block_intrinsic_width
        try:
            intrinsic = _measure_inline_block_intrinsic_width(
                child, child.style or {})
            return max(0.0, intrinsic)
        except Exception:
            return 0.0
    else:
        height_str = _get_style(child, 'height', 'auto')
        if height_str not in ('auto', ''):
            try:
                return _parse_px(height_str)
            except Exception as _exc:
                _logger.debug("Ignored: %s", _exc)
        return child_box.content_height


def _layout_child(child, container, ctx):
    """Layout a flex child using the correct engine for its display type."""
    from layout.block import BlockLayout
    display = _get_style(child, 'display', 'block')
    if display == 'flex':
        return FlexLayout().layout(child, container, ctx)
    elif display == 'grid':
        from layout.grid import GridLayout
        return GridLayout().layout(child, container, ctx)
    elif display == 'table':
        from layout.table import TableLayout
        return TableLayout().layout(child, container, ctx)
    else:
        from layout.block import BlockLayout
        return BlockLayout().layout(child, container, ctx)


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
        height_str = _get_style(node, 'height', 'auto')
        c_width = container.content_width
        box_sizing = _get_style(node, 'box-sizing', 'content-box')

        if width_str == 'auto' or width_str == '':
            content_width = max(0.0, c_width - margin.left - margin.right
                                - padding.left - padding.right
                                - border_w.left - border_w.right)
        elif width_str.endswith('%'):
            pct = float(width_str[:-1]) / 100
            content_width = c_width * pct
        else:
            raw_w = _parse_px(width_str)
            if box_sizing == 'border-box':
                content_width = max(0.0, raw_w - padding.left - padding.right
                                    - border_w.left - border_w.right)
            else:
                content_width = raw_w

        # Determine explicit container height (for align-items, flex-grow column)
        if height_str not in ('auto', ''):
            if height_str.endswith('%'):
                container_height = container.content_height * float(height_str[:-1]) / 100.0
            else:
                raw_h = _parse_px(height_str)
                if box_sizing == 'border-box':
                    container_height = max(0.0, raw_h - padding.top - padding.bottom
                                           - border_w.top - border_w.bottom)
                else:
                    container_height = raw_h
        else:
            container_height = None  # auto — determined by content

        box.x = container.x + margin.left + border_w.left + padding.left
        box.y = container.y + container.content_height + margin.top + border_w.top + padding.top
        box.content_width = content_width
        box.margin = margin
        box.padding = padding
        box.border = border_w

        direction = _get_style(node, 'flex-direction', 'row')
        wrap = _get_style(node, 'flex-wrap', 'nowrap')
        justify = _get_style(node, 'justify-content', 'flex-start')
        align = _get_style(node, 'align-items', 'stretch')

        flex_children = [c for c in node.children
                         if isinstance(c, Element) and _get_style(c, 'display') != 'none']

        # Sort by CSS order property
        flex_children.sort(key=lambda c: int(_get_style(c, 'order', '0') or '0'))

        if not flex_children:
            box.content_height = container_height or 0.0
            return box

        # Initial layout pass: measure each child's intrinsic/specified size
        child_boxes = []
        for child in flex_children:
            child_container = BoxModel()
            child_container.x = box.x
            child_container.y = box.y
            child_container.content_width = box.content_width
            child_container.content_height = 0.0
            cb = _layout_child(child, child_container, ctx)
            child_boxes.append(cb)

        if direction in ('row', 'row-reverse'):
            self._layout_row(node, box, flex_children, child_boxes,
                             direction, wrap, justify, align,
                             content_width, container_height, ctx)
        else:
            self._layout_column(node, box, flex_children, child_boxes,
                                direction, wrap, justify, align,
                                content_width, container_height, ctx)

        return box

    def _layout_row(self, node, box, flex_children, child_boxes,
                    direction, wrap, justify, align,
                    content_width, container_height, ctx):
        from layout.block import BlockLayout

        col_gap = _parse_gap(node, 'column')
        row_gap = _parse_gap(node, 'row')
        n = len(child_boxes)

        # Compute initial main sizes using flex-basis
        main_sizes = []
        for i, (child, cb) in enumerate(zip(flex_children, child_boxes)):
            main_sizes.append(_initial_main_size(child, cb, content_width, direction))

        # --- flex-wrap: split into lines BEFORE grow/shrink ---
        if wrap == 'wrap':
            lines = self._wrap_row(flex_children, child_boxes, main_sizes,
                                   content_width, col_gap)
        else:
            lines = [(list(range(n)), )]

        all_line_data = []
        y_cursor = box.y

        for line_info in lines:
            indices = line_info[0]
            line_children = [flex_children[i] for i in indices]
            line_boxes = [child_boxes[i] for i in indices]
            line_main = [main_sizes[i] for i in indices]
            n_line = len(line_children)

            # Compute flex-grow / flex-shrink for this line
            total_main = sum(line_main)
            gaps_total = col_gap * max(0, n_line - 1)
            remaining = content_width - total_main - gaps_total

            flex_grows = [float(_get_style(c, 'flex-grow', '0') or '0')
                          for c in line_children]
            flex_shrinks = [float(_get_style(c, 'flex-shrink', '1') or '1')
                           for c in line_children]

            total_grow = sum(flex_grows)
            total_weighted_shrink = sum(
                flex_shrinks[i] * max(line_main[i], 1.0)
                for i in range(n_line)
            )

            final_widths = list(line_main)

            if remaining > 0 and total_grow > 0:
                for i in range(n_line):
                    if flex_grows[i] > 0:
                        final_widths[i] += remaining * (flex_grows[i] / total_grow)
            elif remaining < 0 and total_weighted_shrink > 0:
                excess = -remaining
                for i in range(n_line):
                    if flex_shrinks[i] > 0:
                        shrink_ratio = (flex_shrinks[i] * max(line_main[i], 1.0)) / total_weighted_shrink
                        final_widths[i] = max(0.0, final_widths[i] - excess * shrink_ratio)

            # Re-layout children with final widths
            for i, (child, old_cb) in enumerate(zip(line_children, line_boxes)):
                if abs(final_widths[i] - old_cb.content_width) > 0.5:
                    child_container = BoxModel()
                    child_container.x = box.x
                    child_container.y = y_cursor
                    child_container.content_width = final_widths[i]
                    child_container.content_height = 0.0
                    new_cb = _layout_child(child, child_container, ctx)
                    new_cb.content_width = final_widths[i]
                    line_boxes[i] = new_cb

            # Line height
            line_height = max(
                (cb.content_height + cb.padding.top + cb.padding.bottom
                 + cb.border.top + cb.border.bottom
                 + cb.margin.top + cb.margin.bottom
                 for cb in line_boxes),
                default=0.0
            )

            # Use container height for single-line if specified
            if container_height is not None and len(lines) == 1:
                line_height = max(line_height, container_height)

            # Position items on this line
            total_line_width = sum(
                cb.content_width + cb.margin.left + cb.margin.right
                + cb.padding.left + cb.padding.right
                + cb.border.left + cb.border.right
                for cb in line_boxes
            )
            slack = content_width - total_line_width - gaps_total

            x_positions = _compute_x_positions(justify, line_boxes, slack, col_gap, box.x)
            if direction == 'row-reverse':
                x_positions = list(reversed(x_positions))

            for i, (child, cb) in enumerate(zip(line_children, line_boxes)):
                cb.x = x_positions[i]

                child_align = _get_style(child, 'align-self', 'auto')
                effective_align = align if child_align in ('auto', '') else child_align
                item_outer_h = (cb.content_height + cb.padding.top + cb.padding.bottom
                                + cb.border.top + cb.border.bottom)

                if effective_align == 'center':
                    cb.y = y_cursor + (line_height - item_outer_h) / 2 + cb.border.top + cb.padding.top
                elif effective_align == 'flex-end':
                    cb.y = y_cursor + line_height - item_outer_h - cb.margin.bottom + cb.border.top + cb.padding.top
                elif effective_align == 'stretch':
                    stretch_h = max(0.0, line_height - cb.margin.top - cb.margin.bottom
                                    - cb.padding.top - cb.padding.bottom
                                    - cb.border.top - cb.border.bottom)
                    cb.content_height = stretch_h
                    cb.y = y_cursor + cb.margin.top + cb.border.top + cb.padding.top
                else:  # flex-start
                    cb.y = y_cursor + cb.margin.top + cb.border.top + cb.padding.top
                child.box = cb

            y_cursor += line_height
            if len(lines) > 1:
                y_cursor += row_gap

        box.content_height = y_cursor - box.y

    def _layout_column(self, node, box, flex_children, child_boxes,
                       direction, wrap, justify, align,
                       content_width, container_height, ctx):
        from layout.block import BlockLayout

        row_gap = _parse_gap(node, 'row')
        n = len(child_boxes)

        # Compute initial main sizes
        main_sizes = []
        for i, (child, cb) in enumerate(zip(flex_children, child_boxes)):
            main_sizes.append(_initial_main_size(child, cb, content_width, direction))

        # Compute flex-grow / flex-shrink
        total_main = sum(main_sizes)
        gaps_total = row_gap * max(0, n - 1)
        available = container_height if container_height is not None else total_main + gaps_total
        remaining = available - total_main - gaps_total

        flex_grows = [float(_get_style(c, 'flex-grow', '0') or '0') for c in flex_children]
        flex_shrinks = [float(_get_style(c, 'flex-shrink', '1') or '1') for c in flex_children]
        total_grow = sum(flex_grows)

        final_heights = list(main_sizes)

        if remaining > 0 and total_grow > 0:
            for i in range(n):
                if flex_grows[i] > 0:
                    final_heights[i] += remaining * (flex_grows[i] / total_grow)

        # Position children
        y = box.y
        if direction == 'column-reverse':
            # Start from bottom
            y = box.y + (container_height or sum(final_heights) + gaps_total)
            for i, (child, cb) in enumerate(zip(flex_children, child_boxes)):
                y -= final_heights[i]
                cb.x = box.x + cb.margin.left
                cb.y = y
                cb.content_height = final_heights[i]
                child.box = cb
                if i < n - 1:
                    y -= row_gap
            box.content_height = container_height or (sum(final_heights) + gaps_total)
        else:
            for i, (child, cb) in enumerate(zip(flex_children, child_boxes)):
                cb.x = box.x + cb.margin.left
                cb.y = y + cb.margin.top
                cb.content_height = final_heights[i]
                y += final_heights[i] + cb.margin.top + cb.margin.bottom
                if i < n - 1:
                    y += row_gap
                child.box = cb
            box.content_height = y - box.y

    def _wrap_row(self, flex_children, child_boxes, main_sizes,
                  content_width, gap):
        """Break items into wrap lines, returns list of (indices_list,)."""
        lines = []
        current = []
        current_width = 0.0

        for i in range(len(flex_children)):
            item_w = main_sizes[i]
            cb = child_boxes[i]
            outer_w = item_w + cb.margin.left + cb.margin.right
            if current:
                needed = current_width + gap + outer_w
            else:
                needed = outer_w

            if current and needed > content_width:
                lines.append((current,))
                current = [i]
                current_width = outer_w
            else:
                current.append(i)
                current_width = needed

        if current:
            lines.append((current,))
        return lines


def _compute_x_positions(justify: str, child_boxes: list, slack: float, gap: float, base_x: float) -> list:
    """Compute x positions for a single flex line."""
    n = len(child_boxes)
    if justify in ('flex-start', 'start'):
        x_positions = []
        x = base_x
        for i, cb in enumerate(child_boxes):
            x_positions.append(x + cb.margin.left + cb.border.left + cb.padding.left)
            x += (cb.content_width + cb.margin.left + cb.margin.right
                  + cb.padding.left + cb.padding.right
                  + cb.border.left + cb.border.right + gap)
    elif justify in ('flex-end', 'end'):
        x_positions = []
        x = base_x + slack
        for cb in child_boxes:
            x_positions.append(x + cb.margin.left + cb.border.left + cb.padding.left)
            x += (cb.content_width + cb.margin.left + cb.margin.right
                  + cb.padding.left + cb.padding.right
                  + cb.border.left + cb.border.right + gap)
    elif justify == 'center':
        x_positions = []
        x = base_x + slack / 2
        for cb in child_boxes:
            x_positions.append(x + cb.margin.left + cb.border.left + cb.padding.left)
            x += (cb.content_width + cb.margin.left + cb.margin.right
                  + cb.padding.left + cb.padding.right
                  + cb.border.left + cb.border.right + gap)
    elif justify == 'space-between':
        if n == 1:
            x_positions = [base_x + child_boxes[0].margin.left + child_boxes[0].border.left + child_boxes[0].padding.left]
        else:
            between_gap = (slack + gap * (n - 1)) / (n - 1) if n > 1 else 0
            x_positions = []
            x = base_x
            for cb in child_boxes:
                x_positions.append(x + cb.margin.left + cb.border.left + cb.padding.left)
                x += (cb.content_width + cb.margin.left + cb.margin.right
                      + cb.padding.left + cb.padding.right
                      + cb.border.left + cb.border.right + between_gap)
    elif justify == 'space-around':
        around_gap = (slack + gap * (n - 1)) / n if n > 0 else 0
        x_positions = []
        x = base_x + around_gap / 2
        for cb in child_boxes:
            x_positions.append(x + cb.margin.left + cb.border.left + cb.padding.left)
            x += (cb.content_width + cb.margin.left + cb.margin.right
                  + cb.padding.left + cb.padding.right
                  + cb.border.left + cb.border.right + around_gap)
    else:
        x_positions = []
        x = base_x
        for cb in child_boxes:
            x_positions.append(x + cb.margin.left + cb.border.left + cb.padding.left)
            x += (cb.content_width + cb.margin.left + cb.margin.right
                  + cb.padding.left + cb.padding.right
                  + cb.border.left + cb.border.right + gap)
    return x_positions


def layout_flex(node, container_box: BoxModel) -> BoxModel:
    from layout.context import LayoutContext
    ctx = LayoutContext()
    return FlexLayout().layout(node, container_box, ctx)
