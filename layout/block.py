"""Block Formatting Context layout."""
from layout.box import BoxModel, EdgeSizes, Rect
from layout.text import _parse_px
from layout.float_manager import FloatManager

# Displays that generate a block-level box and participate in BFC
_BLOCK_DISPLAYS = frozenset({
    'block', 'flex', 'grid', 'table', 'list-item',
    'table-row', 'table-cell', 'table-header-group',
    'table-footer-group', 'table-row-group',
    'table-caption', 'table-column-group',
})


def _shift_subtree(node, dx: float, dy: float) -> None:
    """Recursively shift all box and line_box coordinates in a subtree by (dx, dy)."""
    from html.dom import Element
    for child in node.children:
        if not isinstance(child, Element):
            continue
        if hasattr(child, 'box') and child.box is not None:
            child.box.x += dx
            child.box.y += dy
            child.box.update_legacy()
        if hasattr(child, 'line_boxes'):
            for lb in child.line_boxes:
                lb.x += dx
                lb.y += dy
                for item in lb.items:
                    item.x += dx
                    item.y += dy
        _shift_subtree(child, dx, dy)


def _get_style(node, prop: str, default='') -> str:
    if hasattr(node, 'style') and node.style:
        return node.style.get(prop, default)
    return default


def _parse_edge(node, prop_prefix: str, container_width: float = 0.0) -> EdgeSizes:
    """Parse top/right/bottom/left values for margin or padding.

    Percentage values are resolved against container_width (per CSS spec,
    both horizontal and vertical margins/paddings use the containing block width).
    """
    edges = EdgeSizes()
    for side, attr in [('top', 'top'), ('right', 'right'), ('bottom', 'bottom'), ('left', 'left')]:
        val = _get_style(node, f'{prop_prefix}-{attr}', '0px')
        if val and val.endswith('%') and container_width > 0:
            try:
                edges.__dict__[side] = float(val[:-1]) / 100.0 * container_width
            except ValueError:
                edges.__dict__[side] = 0.0
        else:
            edges.__dict__[side] = _parse_px(val)
    return edges


def _parse_offset(val: str, fallback: float = 0.0) -> float:
    """Parse a CSS offset value (top/left/right/bottom). Returns fallback for 'auto'."""
    if not val or val == 'auto':
        return fallback
    return _parse_px(val)


def layout_absolute(child, containing_box: BoxModel, position: str,
                     viewport_width: int = 980, viewport_height: int = 600) -> BoxModel:
    """Layout an absolutely or fixed positioned element."""
    from layout.block import layout_block

    style = child.style or {}
    c_width_for_pct = containing_box.content_width if position != 'fixed' else float(viewport_width)
    margin = _parse_edge(child, 'margin', c_width_for_pct)
    padding = _parse_edge(child, 'padding', c_width_for_pct)
    border_w = _parse_edge(child, 'border-width', c_width_for_pct)

    # Resolve width
    width_str = style.get('width', 'auto')
    if position == 'fixed':
        c_width = float(viewport_width)
        c_x = 0.0
        c_y = 0.0
    else:
        c_width = containing_box.content_width
        c_x = containing_box.x
        c_y = containing_box.y

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

    # Build a temporary container to measure content height
    temp_container = BoxModel()
    temp_container.x = c_x + margin.left + border_w.left + padding.left
    temp_container.y = c_y
    temp_container.content_width = content_width
    temp_container.content_height = 0.0

    cb = layout_block(child, temp_container)

    # Resolve height
    height_str = style.get('height', 'auto')
    if height_str and height_str != 'auto':
        if height_str.endswith('%') and position == 'fixed':
            content_height = float(viewport_height) * float(height_str[:-1]) / 100
        else:
            content_height = _parse_px(height_str)
        cb.content_height = content_height

    total_w = content_width + padding.left + padding.right + border_w.left + border_w.right
    total_h = cb.content_height + padding.top + padding.bottom + border_w.top + border_w.bottom

    # Resolve position
    top_val = style.get('top', 'auto')
    left_val = style.get('left', 'auto')
    right_val = style.get('right', 'auto')
    bottom_val = style.get('bottom', 'auto')

    if position == 'fixed':
        ref_x = 0.0
        ref_y = 0.0
        ref_w = float(viewport_width)
    else:
        ref_x = c_x
        ref_y = c_y
        ref_w = c_width

    # Determine x
    if left_val != 'auto':
        x = ref_x + _parse_px(left_val) + margin.left + border_w.left + padding.left
    elif right_val != 'auto':
        x = ref_x + ref_w - _parse_px(right_val) - total_w + margin.left + border_w.left + padding.left
    else:
        x = ref_x + margin.left + border_w.left + padding.left

    # Determine y
    if top_val != 'auto':
        y = ref_y + _parse_px(top_val) + margin.top + border_w.top + padding.top
    elif bottom_val != 'auto':
        y = ref_y + float(viewport_height) - _parse_px(bottom_val) - total_h + margin.top + border_w.top + padding.top
    else:
        y = ref_y + margin.top + border_w.top + padding.top

    cb.x = x
    cb.y = y
    cb.content_width = content_width
    cb.margin = margin
    cb.padding = padding
    cb.border = border_w
    cb.update_legacy()
    return cb


def layout_block(node, container_box: BoxModel, float_mgr: FloatManager = None,
                 viewport_width: int = 980) -> BoxModel:
    """Layout a block-level element. Returns the element's BoxModel."""
    from html.dom import Element, Text, Document

    if float_mgr is None:
        float_mgr = FloatManager()

    box = BoxModel()

    # --- Determine width ---
    width_str = _get_style(node, 'width', 'auto')
    box_sizing = _get_style(node, 'box-sizing', 'content-box')
    c_width = container_box.content_width
    margin = _parse_edge(node, 'margin', c_width)
    padding = _parse_edge(node, 'padding', c_width)
    border_w = _parse_edge(node, 'border-width', c_width)

    if width_str == 'auto' or width_str == '':
        # <img> with auto width uses natural width as intrinsic size
        nat_w = getattr(node, 'natural_width', 0) or 0
        if getattr(node, 'tag', '') == 'img' and nat_w > 0:
            content_width = float(nat_w)
        else:
            content_width = max(0.0, c_width - margin.left - margin.right
                                - padding.left - padding.right
                                - border_w.left - border_w.right)
    elif width_str.endswith('%'):
        pct = float(width_str[:-1]) / 100
        border_box_w = c_width * pct
        if box_sizing == 'border-box':
            content_width = max(0.0, border_box_w - padding.left - padding.right
                                - border_w.left - border_w.right)
        else:
            content_width = max(0.0, border_box_w - margin.left - margin.right
                                - padding.left - padding.right
                                - border_w.left - border_w.right)
    else:
        raw_w = _parse_px(width_str)
        if box_sizing == 'border-box':
            content_width = max(0.0, raw_w - padding.left - padding.right
                                - border_w.left - border_w.right)
        else:
            content_width = max(0.0, raw_w)

    # Apply min-width / max-width constraints
    min_w_str = _get_style(node, 'min-width', '')
    max_w_str = _get_style(node, 'max-width', '')
    if min_w_str and min_w_str not in ('none', ''):
        try:
            min_w = c_width * float(min_w_str[:-1]) / 100 if min_w_str.endswith('%') else _parse_px(min_w_str)
            content_width = max(content_width, min_w)
        except Exception:
            pass
    if max_w_str and max_w_str not in ('none', ''):
        try:
            max_w = c_width * float(max_w_str[:-1]) / 100 if max_w_str.endswith('%') else _parse_px(max_w_str)
            content_width = min(content_width, max_w)
        except Exception:
            pass

    # Handle margin:auto for centering when explicit width
    margin_left_str = _get_style(node, 'margin-left', '0px')
    margin_right_str = _get_style(node, 'margin-right', '0px')
    if (margin_left_str == 'auto' or margin_right_str == 'auto') and width_str not in ('auto', ''):
        total_non_content = (padding.left + padding.right +
                             border_w.left + border_w.right)
        remaining = max(0.0, c_width - content_width - total_non_content -
                        (margin.left if margin_left_str != 'auto' else 0) -
                        (margin.right if margin_right_str != 'auto' else 0))
        if margin_left_str == 'auto' and margin_right_str == 'auto':
            auto_margin = remaining / 2
            box_x = container_box.x + auto_margin + border_w.left + padding.left
        elif margin_left_str == 'auto':
            box_x = container_box.x + remaining + border_w.left + padding.left
        else:
            box_x = container_box.x + margin.left + border_w.left + padding.left
    else:
        box_x = container_box.x + margin.left + border_w.left + padding.left

    # Position
    box.x = box_x
    box.y = container_box.y + container_box.content_height + margin.top
    box.content_width = content_width
    box.margin = margin
    box.padding = padding
    box.border = border_w

    # --- Classify children: block-level vs inline-level ---
    from html.dom import Text as TextNode
    has_block_children = any(
        isinstance(c, Element) and _get_style(c, 'display', 'inline') in _BLOCK_DISPLAYS
        for c in node.children
    )
    has_inline_content = any(
        isinstance(c, TextNode) or
        (isinstance(c, Element) and _get_style(c, 'display', 'inline') not in _BLOCK_DISPLAYS
         and _get_style(c, 'display', 'inline') != 'none'
         and _get_style(c, 'float', 'none') == 'none')
        for c in node.children
    )

    # --- Layout block-level children (BFC) ---
    child_y_offset = 0.0
    prev_margin_bottom = 0.0  # for margin collapsing
    abs_children = []  # collect absolutely / fixed positioned children

    for child in node.children:
        if not isinstance(child, Element):
            continue  # text nodes handled in inline pass

        child_display = _get_style(child, 'display', 'inline')
        child_float = _get_style(child, 'float', 'none')
        child_position = _get_style(child, 'position', 'static')

        if child_display == 'none':
            continue

        # Absolutely / fixed positioned elements are taken out of normal flow
        if child_position in ('absolute', 'fixed'):
            if not hasattr(node, '_abs_children'):
                node._abs_children = []
            node._abs_children.append((child, child_position))
            abs_children.append((child, child_position))
            continue

        # Inline elements are handled in the inline layout pass below
        if child_display not in _BLOCK_DISPLAYS and child_float == 'none':
            continue

        # clear: push y offset past relevant floats
        clear_val = _get_style(child, 'clear', 'none')
        if clear_val != 'none' and float_mgr:
            clear_y_pos = float_mgr.clear_y(clear_val)
            if clear_y_pos > box.y + child_y_offset:
                child_y_offset = clear_y_pos - box.y

        if child_float != 'none':
            # Float element: lay out in a temporary container at origin (0, 0)
            temp_container = BoxModel()
            temp_container.x = 0.0
            temp_container.y = 0.0
            temp_container.content_width = box.content_width
            temp_container.content_height = 0.0
            # Floated elements establish a new BFC — use a fresh FloatManager
            # so inner floats don't see outer ones.
            child_box = layout_block(child, temp_container, FloatManager(), viewport_width)

            # Determine where the float's content box should actually be placed
            float_x, float_w = float_mgr.available_rect(
                box.y + child_y_offset, child_box.content_height, box.x, box.content_width)
            if child_float == 'left':
                new_x = float_x + child_box.margin.left + child_box.border.left + child_box.padding.left
            else:
                new_x = (float_x + float_w
                         - child_box.content_width
                         - child_box.margin.right - child_box.border.right - child_box.padding.right)
            new_y = (box.y + child_y_offset
                     + child_box.margin.top + child_box.border.top + child_box.padding.top)

            # Compute the shift needed and apply it to all descendants
            dx = new_x - child_box.x
            dy = new_y - child_box.y
            child_box.x = new_x
            child_box.y = new_y
            child_box.update_legacy()
            if dx != 0.0 or dy != 0.0:
                _shift_subtree(child, dx, dy)

            child.box = child_box

            # Register with the float manager using the border box
            fm_x = child_box.x - child_box.padding.left - child_box.border.left
            fm_y = child_box.y - child_box.padding.top - child_box.border.top - child_box.margin.top
            fm_w = (child_box.content_width
                    + child_box.padding.left + child_box.padding.right
                    + child_box.border.left + child_box.border.right)
            fm_h = (child_box.content_height
                    + child_box.padding.top + child_box.padding.bottom
                    + child_box.border.top + child_box.border.bottom)
            float_mgr.add_float(fm_x, fm_y, fm_w, fm_h, child_float)
        else:
            # Block child
            child_container = BoxModel()
            child_container.x = box.x
            child_container.y = box.y + child_y_offset
            child_container.content_width = box.content_width
            child_container.content_height = 0.0

            if child_display == 'flex':
                from layout.flex import layout_flex
                child_box = layout_flex(child, child_container)
            elif child_display == 'grid':
                from layout.grid import layout_grid
                child_box = layout_grid(child, child_container)
            elif child_display == 'table':
                from layout.table import layout_table
                child_box = layout_table(child, child_container, viewport_width)
            else:
                child_box = layout_block(child, child_container, float_mgr, viewport_width)
            child.box = child_box
            child_box.update_legacy()

            # Margin collapsing (adjacent siblings)
            top_margin = child_box.margin.top
            collapsed = max(prev_margin_bottom, top_margin)
            child_box.y = box.y + child_y_offset + collapsed
            child_box.update_legacy()

            child_height = (child_box.content_height
                            + child_box.padding.top + child_box.padding.bottom
                            + child_box.border.top + child_box.border.bottom
                            + collapsed)
            child_y_offset += child_height
            prev_margin_bottom = child_box.margin.bottom

    # --- Handle inline content (text nodes + inline elements) ---
    if has_inline_content:
        from layout.inline import layout_inline
        lines, inline_height = layout_inline(
            node, box.x, box.y + child_y_offset, box.content_width, float_mgr)
        node.line_boxes = lines
        child_y_offset += inline_height

    # --- Determine height ---
    height_str = _get_style(node, 'height', 'auto')
    if height_str == 'auto' or height_str == '':
        # For <img> with auto height, scale from natural dimensions
        if (hasattr(node, 'tag') and node.tag == 'img'
                and child_y_offset == 0.0):
            nat_h = getattr(node, 'natural_height', 0) or 0
            nat_w = getattr(node, 'natural_width', 0) or 0
            if nat_h > 0:
                if content_width > 0 and nat_w > 0:
                    content_height = nat_h * content_width / nat_w
                else:
                    content_height = float(nat_h)
            else:
                content_height = child_y_offset
        else:
            content_height = child_y_offset
    elif height_str.endswith('%'):
        content_height = child_y_offset
    else:
        content_height = _parse_px(height_str)

    # Apply min-height / max-height constraints
    min_h_str = _get_style(node, 'min-height', '')
    max_h_str = _get_style(node, 'max-height', '')
    if min_h_str and min_h_str not in ('none', ''):
        try:
            content_height = max(content_height, _parse_px(min_h_str))
        except Exception:
            pass
    if max_h_str and max_h_str not in ('none', ''):
        try:
            content_height = min(content_height, _parse_px(max_h_str))
        except Exception:
            pass

    box.content_height = max(content_height, 0.0)
    box.update_legacy()

    # --- Layout absolutely/fixed positioned children ---
    for child, child_position in abs_children:
        child_box = layout_absolute(
            child, box, child_position,
            viewport_width=viewport_width,
        )
        child.box = child_box
        child_box.update_legacy()

    return box
