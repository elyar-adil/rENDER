"""Block Formatting Context layout."""
from __future__ import annotations
from layout.box import BoxModel, EdgeSizes, Rect
from layout.text import _parse_px
from layout.float_manager import FloatManager
from layout.context import LayoutEngine, LayoutContext

_BLOCK_DISPLAYS = frozenset({
    'block', 'flex', 'grid', 'table', 'list-item',
    'table-row', 'table-cell', 'table-header-group',
    'table-footer-group', 'table-row-group',
    'table-caption', 'table-column-group',
})


def _resolve_replaced_img_dim(css_val: str, attr_val: str, natural: int) -> float:
    """Resolve an <img> width/height from CSS, HTML attribute, or intrinsic size."""
    for val in (css_val, attr_val):
        if not val or val in ('auto', ''):
            continue
        try:
            if isinstance(val, str):
                raw = val.strip()
                if raw.endswith('px'):
                    return max(0.0, float(raw[:-2]))
                if raw.lstrip('-').isdigit():
                    return max(0.0, float(raw))
            return max(0.0, float(_parse_px(val)))
        except Exception:
            pass
    return float(natural or 0)


def _resolve_replaced_img_width(node, style: dict, c_width: float,
                                margin: EdgeSizes, padding: EdgeSizes,
                                border_w: EdgeSizes, box_sizing: str) -> float:
    """Resolve the used content width of a block-level <img>."""
    nat_w = getattr(node, 'natural_width', 0) or 0
    nat_h = getattr(node, 'natural_height', 0) or 0

    raw_width = style.get('width', 'auto')
    width_attr = node.attributes.get('width', '')
    width = _resolve_replaced_img_dim(raw_width, width_attr, 0)

    if width <= 0.0:
        raw_height = style.get('height', 'auto')
        height_attr = node.attributes.get('height', '')
        resolved_h = _resolve_replaced_img_dim(raw_height, height_attr, 0)
        if resolved_h > 0.0 and nat_w > 0 and nat_h > 0:
            width = resolved_h * nat_w / nat_h

    if raw_width not in ('auto', '') and raw_width.endswith('%'):
        pct = float(raw_width[:-1]) / 100.0
        border_box_w = c_width * pct
        if box_sizing == 'border-box':
            width = max(0.0, border_box_w - padding.left - padding.right
                        - border_w.left - border_w.right)
        else:
            width = max(0.0, border_box_w)

    if width <= 0.0:
        if nat_w > 0:
            width = float(nat_w)
        else:
            width = 0.0

    if width <= 0.0:
        width = max(0.0, c_width - margin.left - margin.right
                    - padding.left - padding.right
                    - border_w.left - border_w.right)

    return float(width)


def _resolve_replaced_img_height(node, style: dict, content_width: float) -> float:
    """Resolve the used content height of a block-level <img>."""
    nat_w = getattr(node, 'natural_width', 0) or 0
    nat_h = getattr(node, 'natural_height', 0) or 0

    raw_height = style.get('height', 'auto')
    height_attr = node.attributes.get('height', '')
    height = _resolve_replaced_img_dim(raw_height, height_attr, 0)
    if height > 0.0:
        return float(height)

    if nat_w > 0 and nat_h > 0 and content_width > 0.0:
        return float(nat_h) * float(content_width) / float(nat_w)
    return float(nat_h or 0)


def _shift_subtree(node, dx: float, dy: float) -> None:
    from html.dom import Element
    if dx == 0.0 and dy == 0.0:
        return

    if hasattr(node, 'box') and node.box is not None:
        node.box.x += dx
        node.box.y += dy
        node.box.update_legacy()

    if hasattr(node, 'line_boxes'):
        for lb in node.line_boxes:
            lb.x += dx
            lb.y += dy
            for item in lb.items:
                item.x += dx
                item.y += dy
                if item.layout_node is not None and item.layout_node is not node:
                    _shift_subtree(item.layout_node, dx, dy)

    for child in node.children:
        if isinstance(child, Element):
            _shift_subtree(child, dx, dy)


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


def _parse_offset(val: str, fallback: float = 0.0) -> float:
    if not val or val == 'auto':
        return fallback
    return _parse_px(val)


def _resolve_length_against(val: str, reference: float, fallback: float = 0.0) -> float:
    if not val or val == 'auto':
        return fallback
    val = val.strip()
    if val.endswith('%'):
        try:
            return reference * float(val[:-1]) / 100.0
        except ValueError:
            return fallback
    return _parse_px(val)


def _collapse_adjacent_margins(previous_bottom: float, current_top: float) -> float:
    """Collapse adjoining vertical margins, including negative values."""
    if previous_bottom >= 0 and current_top >= 0:
        return max(previous_bottom, current_top)
    if previous_bottom <= 0 and current_top <= 0:
        return min(previous_bottom, current_top)
    return previous_bottom + current_top


def _measure_auto_width(node, container_width: float) -> float:
    try:
        from html.dom import Element, Text
        from layout.inline import _measure_inline_block_intrinsic_width, _measure_text_span
        style = getattr(node, 'style', {}) or {}
        measured = 0.0
        for child in getattr(node, 'children', []):
            if isinstance(child, Element):
                measured += _measure_inline_block_intrinsic_width(child, child.style or style)
            elif isinstance(child, Text):
                text = ' '.join(child.data.split())
                if text:
                    measured += _measure_text_span(text, style)
        if measured > 0:
            return measured
    except Exception:
        pass
    return max(0.0, container_width)


class BlockLayout(LayoutEngine):
    """Layout engine for display:block (and fallback for unknown display values)."""

    def layout(self, node, container: BoxModel, ctx: LayoutContext) -> BoxModel:
        from html.dom import Element, Text as TextNode

        box = BoxModel()
        c_width = container.content_width
        width_str = _get_style(node, 'width', 'auto')
        box_sizing = _get_style(node, 'box-sizing', 'content-box')
        margin = _parse_edge(node, 'margin', c_width)
        padding = _parse_edge(node, 'padding', c_width)
        border_w = _parse_edge(node, 'border-width', c_width)
        is_replaced_img = getattr(node, 'tag', '') == 'img'

        # --- Width ---
        if is_replaced_img:
            content_width = _resolve_replaced_img_width(
                node, getattr(node, 'style', {}) or {}, c_width,
                margin, padding, border_w, box_sizing,
            )
        elif width_str in ('auto', ''):
            # <img> with auto width → use natural width
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

        # min/max width
        for prop, op in (('min-width', max), ('max-width', min)):
            s = _get_style(node, prop, '')
            if s and s not in ('none', ''):
                try:
                    v = c_width * float(s[:-1]) / 100 if s.endswith('%') else _parse_px(s)
                    content_width = op(content_width, v)
                except Exception:
                    pass

        # --- margin:auto centering ---
        ml_str = _get_style(node, 'margin-left', '0px')
        mr_str = _get_style(node, 'margin-right', '0px')
        used_width_known = (
            (width_str not in ('auto', ''))
            or (is_replaced_img and content_width > 0.0)
        )
        if (ml_str == 'auto' or mr_str == 'auto') and used_width_known:
            non_content = (padding.left + padding.right + border_w.left + border_w.right
                           + (margin.left if ml_str != 'auto' else 0)
                           + (margin.right if mr_str != 'auto' else 0))
            remaining = max(0.0, c_width - content_width - non_content)
            if ml_str == 'auto' and mr_str == 'auto':
                box_x = container.x + remaining / 2 + border_w.left + padding.left
            elif ml_str == 'auto':
                box_x = container.x + remaining + border_w.left + padding.left
            else:
                box_x = container.x + margin.left + border_w.left + padding.left
        else:
            box_x = container.x + margin.left + border_w.left + padding.left

        box.x = box_x
        box.y = container.y + container.content_height + margin.top + border_w.top + padding.top
        box.content_width = content_width
        box.margin = margin
        box.padding = padding
        box.border = border_w

        # --- Classify children ---
        has_block = any(
            isinstance(c, Element)
            and _get_style(c, 'display', 'inline') in _BLOCK_DISPLAYS
            for c in node.children
        )
        has_inline = any(
            isinstance(c, TextNode) or
            (isinstance(c, Element)
             and _get_style(c, 'display', 'inline') not in _BLOCK_DISPLAYS
             and _get_style(c, 'display', 'inline') != 'none'
             and _get_style(c, 'float', 'none') == 'none'
             and _get_style(c, 'position', 'static') not in ('absolute', 'fixed'))
            for c in node.children
        )

        child_y = 0.0
        prev_margin_bottom = 0.0
        abs_children = []

        for child in node.children:
            if not isinstance(child, Element):
                continue
            child_display = _get_style(child, 'display', 'inline')
            child_float = _get_style(child, 'float', 'none')
            child_pos = _get_style(child, 'position', 'static')

            if child_display == 'none':
                continue
            if child_pos in ('absolute', 'fixed'):
                if not hasattr(node, '_abs_children'):
                    node._abs_children = []
                node._abs_children.append((child, child_pos))
                abs_children.append((child, child_pos))
                continue
            if child_display not in _BLOCK_DISPLAYS and child_float == 'none':
                continue

            # clear
            clear_val = _get_style(child, 'clear', 'none')
            if clear_val != 'none' and ctx.float_mgr:
                clear_y = ctx.float_mgr.clear_y(clear_val)
                if clear_y > box.y + child_y:
                    child_y = clear_y - box.y

            if child_float != 'none':
                # Float: new BFC for inner content
                tmp = BoxModel()
                tmp.x = 0.0; tmp.y = 0.0
                tmp.content_width = box.content_width
                tmp.content_height = 0.0
                child_ctx = ctx.fork()
                child_box = BlockLayout().layout(child, tmp, child_ctx)
                child.box = child_box
                child_box.update_legacy()

                float_x, float_w = ctx.float_mgr.available_rect(
                    box.y + child_y, child_box.content_height, box.x, box.content_width)
                if child_float == 'left':
                    new_x = float_x + child_box.margin.left + child_box.border.left + child_box.padding.left
                else:
                    new_x = (float_x + float_w - child_box.content_width
                             - child_box.margin.right - child_box.border.right - child_box.padding.right)
                new_y = (box.y + child_y
                         + child_box.margin.top + child_box.border.top + child_box.padding.top)
                dx = new_x - child_box.x
                dy = new_y - child_box.y
                if dx or dy:
                    _shift_subtree(child, dx, dy)
                child.box = child_box

                fm_x = child_box.x - child_box.padding.left - child_box.border.left
                fm_y = child_box.y - child_box.padding.top - child_box.border.top - child_box.margin.top
                fm_w = child_box.content_width + child_box.padding.left + child_box.padding.right + child_box.border.left + child_box.border.right
                fm_h = child_box.content_height + child_box.padding.top + child_box.padding.bottom + child_box.border.top + child_box.border.bottom
                ctx.float_mgr.add_float(fm_x, fm_y, fm_w, fm_h, child_float)
            else:
                child_cont = BoxModel()
                child_cont.x = box.x
                child_cont.y = box.y + child_y
                child_cont.content_width = box.content_width
                child_cont.content_height = 0.0

                if child_display == 'table':
                    from layout.table import TableLayout
                    child_box = TableLayout().layout(child, child_cont, ctx)
                elif child_display == 'flex':
                    from layout.flex import FlexLayout
                    child_box = FlexLayout().layout(child, child_cont, ctx)
                elif child_display == 'grid':
                    from layout.grid import GridLayout
                    child_box = GridLayout().layout(child, child_cont, ctx)
                else:
                    child_box = BlockLayout().layout(child, child_cont, ctx)
                child.box = child_box
                child_box.update_legacy()

                top_margin = child_box.margin.top
                collapsed = _collapse_adjacent_margins(prev_margin_bottom, top_margin)
                child_box.y += collapsed - top_margin
                child_box.update_legacy()

                child_h = (child_box.content_height + child_box.padding.top + child_box.padding.bottom
                           + child_box.border.top + child_box.border.bottom + collapsed)
                child_y += child_h
                prev_margin_bottom = child_box.margin.bottom

        # Inline pass
        if has_inline:
            from layout.inline import layout_inline
            lines, inline_h = layout_inline(node, box.x, box.y + child_y, box.content_width, ctx.float_mgr)
            node.line_boxes = lines
            child_y += inline_h

        # --- Height ---
        height_str = _get_style(node, 'height', 'auto')
        overflow = _get_style(node, 'overflow', 'visible')
        if height_str in ('auto', '') or height_str.endswith('%'):
            content_height = child_y
        else:
            specified_h = _parse_px(height_str)
            # overflow:visible → content can exceed specified height
            content_height = specified_h if overflow not in ('visible', '') else max(specified_h, child_y)

        # min/max height
        for prop, op in (('min-height', max), ('max-height', min)):
            s = _get_style(node, prop, '')
            if s and s not in ('none', ''):
                try:
                    content_height = op(content_height, _parse_px(s))
                except Exception:
                    pass

        # <img> auto height from natural dimensions
        if is_replaced_img and child_y == 0.0:
            resolved_h = _resolve_replaced_img_height(
                node, getattr(node, 'style', {}) or {}, content_width,
            )
            if resolved_h > 0.0:
                content_height = resolved_h

        box.content_height = max(content_height, 0.0)
        box.update_legacy()

        # Absolutely positioned children
        for child, child_pos in abs_children:
            child_box = BlockLayout.layout_absolute(child, box, child_pos,
                                                    ctx.viewport_width, ctx.viewport_height)
            child.box = child_box
            child_box.update_legacy()

        return box

    @staticmethod
    def layout_absolute(child, containing_box: BoxModel, position: str,
                        viewport_width: int = 980, viewport_height: int = 600) -> BoxModel:
        """Layout an absolutely or fixed positioned element."""
        style = child.style or {}
        c_vw = containing_box.content_width if position != 'fixed' else float(viewport_width)
        margin = _parse_edge(child, 'margin', c_vw)
        padding = _parse_edge(child, 'padding', c_vw)
        border_w = _parse_edge(child, 'border-width', c_vw)

        width_str = style.get('width', 'auto')
        box_sizing = style.get('box-sizing', 'content-box')
        is_replaced_img = getattr(child, 'tag', '') == 'img'
        c_width = float(viewport_width) if position == 'fixed' else containing_box.content_width
        c_x = 0.0 if position == 'fixed' else containing_box.x
        c_y = 0.0 if position == 'fixed' else containing_box.y
        c_height = float(viewport_height) if position == 'fixed' else containing_box.content_height
        left_val = style.get('left', 'auto')
        right_val = style.get('right', 'auto')
        top_val = style.get('top', 'auto')
        bottom_val = style.get('bottom', 'auto')
        left_px = _resolve_length_against(left_val, c_width)
        right_px = _resolve_length_against(right_val, c_width)
        top_px = _resolve_length_against(top_val, c_height)
        bottom_px = _resolve_length_against(bottom_val, c_height)

        if is_replaced_img:
            content_width = _resolve_replaced_img_width(
                child, getattr(child, 'style', {}) or {}, c_width,
                margin, padding, border_w, box_sizing,
            )
        elif width_str in ('auto', ''):
            if left_val != 'auto' and right_val != 'auto':
                content_width = max(
                    0.0,
                    c_width - left_px - right_px - margin.left - margin.right
                    - padding.left - padding.right - border_w.left - border_w.right,
                )
            else:
                content_width = _measure_auto_width(child, c_width)
        elif width_str.endswith('%'):
            content_width = max(0.0, c_width * float(width_str[:-1]) / 100
                                - margin.left - margin.right
                                - padding.left - padding.right - border_w.left - border_w.right)
        else:
            content_width = max(0.0, _parse_px(width_str))

        for prop, op in (('min-width', max), ('max-width', min)):
            s = style.get(prop, '')
            if s and s not in ('none', ''):
                try:
                    v = c_width * float(s[:-1]) / 100 if s.endswith('%') else _parse_px(s)
                    content_width = op(content_width, v)
                except Exception:
                    pass

        tmp = BoxModel()
        tmp.x = c_x + margin.left + border_w.left + padding.left
        tmp.y = c_y
        tmp.content_width = content_width
        tmp.content_height = 0.0

        ctx = LayoutContext(viewport_width, viewport_height)
        original_margins = {
            side: style.get(f'margin-{side}')
            for side in ('top', 'right', 'bottom', 'left')
        }
        try:
            for side in ('top', 'right', 'bottom', 'left'):
                style[f'margin-{side}'] = '0px'
            cb = BlockLayout().layout(child, tmp, ctx)
        finally:
            for side, value in original_margins.items():
                key = f'margin-{side}'
                if value is None:
                    style.pop(key, None)
                else:
                    style[key] = value

        height_str = style.get('height', 'auto')
        if height_str and height_str != 'auto':
            if height_str.endswith('%') and position == 'fixed':
                cb.content_height = float(viewport_height) * float(height_str[:-1]) / 100
            else:
                cb.content_height = _parse_px(height_str)

        total_w = content_width + padding.left + padding.right + border_w.left + border_w.right
        total_h = cb.content_height + padding.top + padding.bottom + border_w.top + border_w.bottom

        ref_x = 0.0 if position == 'fixed' else c_x
        ref_y = 0.0 if position == 'fixed' else c_y
        ref_w = float(viewport_width) if position == 'fixed' else c_width
        ref_h = float(viewport_height) if position == 'fixed' else c_height

        x = (ref_x + left_px + margin.left + border_w.left + padding.left
             if left_val != 'auto' else
             ref_x + ref_w - right_px - total_w + margin.left + border_w.left + padding.left
             if right_val != 'auto' else
             ref_x + margin.left + border_w.left + padding.left)
        y = (ref_y + top_px + margin.top + border_w.top + padding.top
             if top_val != 'auto' else
             ref_y + ref_h - bottom_px - total_h + margin.top + border_w.top + padding.top
             if bottom_val != 'auto' else
             ref_y + margin.top + border_w.top + padding.top)

        old_x, old_y = cb.x, cb.y
        cb.content_width = content_width
        cb.margin = margin; cb.padding = padding; cb.border = border_w
        child.box = cb
        dx = x - old_x
        dy = y - old_y
        if dx or dy:
            _shift_subtree(child, dx, dy)
        cb.update_legacy()
        return cb


# ---------------------------------------------------------------------------
# Compatibility shims (keep old function signatures working for __init__.py)
# ---------------------------------------------------------------------------

def layout_block(node, container_box: BoxModel, float_mgr: FloatManager = None,
                 viewport_width: int = 980) -> BoxModel:
    """Backward-compatible wrapper around BlockLayout."""
    ctx = LayoutContext(viewport_width)
    if float_mgr is not None:
        ctx.float_mgr = float_mgr
    return BlockLayout().layout(node, container_box, ctx)


def layout_absolute(child, containing_box: BoxModel, position: str,
                    viewport_width: int = 980, viewport_height: int = 600) -> BoxModel:
    return BlockLayout.layout_absolute(child, containing_box, position,
                                       viewport_width, viewport_height)
