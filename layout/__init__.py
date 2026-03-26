"""Layout engine entry point."""
import logging
_logger = logging.getLogger(__name__)
from layout.box import BoxModel, EdgeSizes, Rect
from layout.block import layout_block, layout_absolute
from layout.flex import layout_flex
from layout.inline import layout_inline
from layout.grid import layout_grid
from rendering.display_list import (
    DisplayList, PushOpacity, PopOpacity, PushTransform, PopTransform,
    DrawBoxShadow, DrawInput, DrawOutline, DrawRadialGradient,
)
from css.utils import split_paren_aware as _split_top_level


VIEWPORT_WIDTH = 980
VIEWPORT_HEIGHT = 600


def _parse_single_shadow(s: str):
    """Parse a single box-shadow value, return (ox, oy, blur, spread, color, inset) or None."""
    import re
    from layout.text import _parse_px
    try:
        s = s.strip()
        inset = False
        if s.startswith('inset'):
            inset = True
            s = s[5:].strip()
        elif s.endswith('inset'):
            inset = True
            s = s[:-5].strip()

        color = 'rgba(0,0,0,0.2)'
        def _extract_color(m):
            nonlocal color
            color = m.group(0)
            return ''
        s = re.sub(r'(rgba?\s*\([^)]+\)|hsla?\s*\([^)]+\))', _extract_color, s)

        parts = s.split()
        offsets = []
        for p in parts:
            p = p.strip(',')
            if not p:
                continue
            try:
                offsets.append(_parse_px(p))
            except Exception:
                color = p
        if len(offsets) < 2:
            return None
        ox = offsets[0]
        oy = offsets[1]
        blur = offsets[2] if len(offsets) >= 3 else 0.0
        spread = offsets[3] if len(offsets) >= 4 else 0.0
        return ox, oy, blur, spread, color, inset
    except Exception:
        return None


def _parse_shadow(shadow_str: str):
    """Parse a box-shadow value, return (ox, oy, blur, spread, color) or None.

    Handles colour functions with spaces like ``rgba(0, 0, 0, 0.2)``.
    For backward compatibility, returns the first non-inset shadow."""
    parts = _split_top_level(shadow_str)
    for part in parts:
        result = _parse_single_shadow(part)
        if result is not None:
            ox, oy, blur, spread, color, inset = result
            if not inset:
                return ox, oy, blur, spread, color
    return None


def _parse_all_shadows(shadow_str: str) -> list:
    """Parse all box-shadow values, return list of (ox, oy, blur, spread, color, inset)."""
    results = []
    for part in _split_top_level(shadow_str):
        result = _parse_single_shadow(part)
        if result is not None:
            results.append(result)
    return results


def _parse_css_transform(transform_str: str):
    """Parse CSS transform value. Return (dx, dy, rotate_deg, scale_x, scale_y) or None."""
    import re
    dx, dy, rot, sx, sy = 0.0, 0.0, 0.0, 1.0, 1.0
    from layout.text import _parse_px
    found = False

    for m in re.finditer(r'(\w+)\s*\(([^)]*)\)', transform_str):
        fn = m.group(1).lower()
        args = [a.strip() for a in m.group(2).split(',')]
        try:
            if fn == 'translate':
                dx += _parse_px(args[0])
                if len(args) > 1:
                    dy += _parse_px(args[1])
                found = True
            elif fn == 'translatex':
                dx += _parse_px(args[0])
                found = True
            elif fn == 'translatey':
                dy += _parse_px(args[0])
                found = True
            elif fn == 'rotate':
                val = args[0].strip()
                if val.endswith('deg'):
                    rot += float(val[:-3])
                elif val.endswith('rad'):
                    rot += float(val[:-3]) * 180 / 3.14159265
                else:
                    rot += float(val)
                found = True
            elif fn == 'scale':
                sx *= float(args[0])
                sy *= float(args[1]) if len(args) > 1 else float(args[0])
                found = True
            elif fn == 'scalex':
                sx *= float(args[0])
                found = True
            elif fn == 'scaley':
                sy *= float(args[0])
                found = True
        except Exception as _exc:
            _logger.debug("Ignored: %s", _exc)
    return (dx, dy, rot, sx, sy) if found else None


def _parse_transform_origin(origin_str: str, box):
    """Parse transform-origin and return (ox, oy) in absolute coordinates."""
    from layout.text import _parse_px
    parts = origin_str.split()
    ox = box.x + box.content_width / 2
    oy = box.y + box.content_height / 2
    try:
        p0 = parts[0] if parts else '50%'
        if p0.endswith('%'):
            ox = box.x + box.content_width * float(p0[:-1]) / 100
        elif p0 == 'left':
            ox = box.x
        elif p0 == 'right':
            ox = box.x + box.content_width
        elif p0 == 'center':
            pass
        else:
            ox = box.x + _parse_px(p0)
    except Exception as _exc:
        _logger.debug("Ignored: %s", _exc)
    try:
        p1 = parts[1] if len(parts) > 1 else '50%'
        if p1.endswith('%'):
            oy = box.y + box.content_height * float(p1[:-1]) / 100
        elif p1 == 'top':
            oy = box.y
        elif p1 == 'bottom':
            oy = box.y + box.content_height
        elif p1 == 'center':
            pass
        else:
            oy = box.y + _parse_px(p1)
    except Exception as _exc:
        _logger.debug("Ignored: %s", _exc)
    return ox, oy


def layout(document, viewport_width: int = VIEWPORT_WIDTH, viewport_height: int = VIEWPORT_HEIGHT) -> 'DisplayList':
    """Layout the document and return a DisplayList of draw commands.

    Also sets document.box and element.box on each node.
    """
    from html.dom import Element, Text, Document

    # Create root box
    root_box = BoxModel()
    root_box.x = 0.0
    root_box.y = 0.0
    root_box.content_width = float(viewport_width)
    root_box.content_height = 0.0

    document.box = root_box

    # Layout all children
    _layout_children(document, root_box, viewport_width)

    # After initial layout, process any absolutely/fixed positioned elements
    # that were deferred during the main layout pass
    _layout_deferred_abs(document, root_box, viewport_width, viewport_height)

    # Build display list — normal elements first, then stacking-context elements on top
    display_list = DisplayList()
    stacking_top: list = []  # (z_index, order, commands) for positioned elements

    _build_display_list(document, display_list, stacking_top)

    # Sort stacking context items by z-index (lower z-index painted first)
    stacking_top.sort(key=lambda item: (item[0], item[1]) if isinstance(item, tuple) and len(item) == 3 else (0, 0))
    for item in stacking_top:
        if isinstance(item, tuple) and len(item) == 3:
            for cmd in item[2]:
                display_list.add(cmd)
        else:
            display_list.add(item)

    return display_list


def _layout_children(node, container_box: BoxModel, viewport_width: int) -> None:
    from html.dom import Element, Text
    from layout.float_manager import FloatManager

    float_mgr = FloatManager()

    for child in node.children:
        if not isinstance(child, Element):
            continue

        display = child.style.get('display', 'block') if hasattr(child, 'style') and child.style else 'block'
        position = child.style.get('position', 'static') if hasattr(child, 'style') and child.style else 'static'

        if display == 'none':
            continue

        # Absolutely/fixed positioned top-level children — defer until after layout
        if position in ('absolute', 'fixed'):
            continue

        if display == 'flex':
            child.box = layout_flex(child, container_box)
        elif display == 'grid':
            child.box = layout_grid(child, container_box)
        elif display == 'table':
            from layout.table import layout_table
            child.box = layout_table(child, container_box, viewport_width)
        else:
            child.box = layout_block(child, container_box, float_mgr, viewport_width)


        # Update container height
        container_box.content_height = max(
            container_box.content_height,
            child.box.y - container_box.y + child.box.content_height + child.box.margin.bottom
        )


def _layout_deferred_abs(node, root_box: BoxModel, viewport_width: int, viewport_height: int) -> None:
    """Walk the DOM and lay out any absolute/fixed elements that weren't laid out yet."""
    from html.dom import Element

    for child in _walk_elements(node):
        position = child.style.get('position', 'static') if hasattr(child, 'style') and child.style else 'static'
        if position in ('absolute', 'fixed') and not hasattr(child, 'box'):
            containing = _find_containing_block(child, node, root_box)
            child.box = layout_absolute(
                child, containing, position,
                viewport_width=viewport_width,
                viewport_height=viewport_height,
            )


def _walk_elements(node):
    """Generator that yields all Element descendants."""
    from html.dom import Element
    for child in node.children:
        if isinstance(child, Element):
            yield child
            yield from _walk_elements(child)


def _find_containing_block(node, root, root_box: BoxModel) -> BoxModel:
    """Find the nearest positioned ancestor's box, or root box for fixed/fallback."""
    from html.dom import Element
    position = node.style.get('position', 'static') if hasattr(node, 'style') and node.style else 'static'
    if position == 'fixed':
        return root_box
    # Walk parent pointers to find nearest positioned ancestor
    parent = getattr(node, 'parent', None)
    while parent is not None and isinstance(parent, Element):
        parent_pos = parent.style.get('position', 'static') if hasattr(parent, 'style') and parent.style else 'static'
        if parent_pos in ('relative', 'absolute', 'fixed', 'sticky'):
            if hasattr(parent, 'box') and parent.box is not None:
                return parent.box
        parent = getattr(parent, 'parent', None)
    return root_box


def _parse_linear_gradient(value: str, rect):
    """Parse a CSS linear-gradient() value.

    Returns (angle_degrees, color_stops) or None on failure.
    color_stops is [(position: float 0..1, color_str), ...].
    """
    import re
    m = re.match(r'linear-gradient\s*\((.+)\)\s*$', value, re.IGNORECASE | re.DOTALL)
    if not m:
        return None

    inner = m.group(1).strip()

    # Split by top-level commas (respecting nested parens)
    parts = []
    depth = 0
    current = []
    for ch in inner:
        if ch == '(':
            depth += 1
            current.append(ch)
        elif ch == ')':
            depth -= 1
            current.append(ch)
        elif ch == ',' and depth == 0:
            parts.append(''.join(current).strip())
            current = []
        else:
            current.append(ch)
    if current:
        parts.append(''.join(current).strip())

    if not parts:
        return None

    # First token may be an angle or direction keyword
    angle = 180.0  # default: top → bottom
    start_idx = 0
    first = parts[0].strip().lower()
    if first.endswith('deg'):
        try:
            angle = float(first[:-3])
            start_idx = 1
        except ValueError:
            pass
    elif first.startswith('to '):
        direction = first[3:].strip()
        angle_map = {
            'bottom': 180.0, 'top': 0.0, 'right': 90.0, 'left': 270.0,
            'bottom right': 135.0, 'bottom left': 225.0,
            'top right': 45.0, 'top left': 315.0,
        }
        angle = angle_map.get(direction, 180.0)
        start_idx = 1
    elif first.endswith('turn'):
        try:
            angle = float(first[:-4]) * 360
            start_idx = 1
        except ValueError:
            pass
    elif first.endswith('rad'):
        try:
            import math as _math
            angle = _math.degrees(float(first[:-3]))
            start_idx = 1
        except ValueError:
            pass

    color_parts = parts[start_idx:]
    if not color_parts:
        return None

    stops = []
    n = len(color_parts)
    for i, part in enumerate(color_parts):
        part = part.strip()
        # Check for trailing position hint like "red 50%" or "blue 100%"
        pos_match = re.search(r'\s+([\d.]+%|[\d.]+px)\s*$', part)
        if pos_match:
            color_str = part[:pos_match.start()].strip()
            pos_str = pos_match.group(1)
            if pos_str.endswith('%'):
                pos = float(pos_str[:-1]) / 100.0
            else:
                try:
                    from layout.text import _parse_px as _ppx
                    pos = _ppx(pos_str) / max(1.0, rect.width)
                except Exception:
                    pos = i / max(1, n - 1)
        else:
            color_str = part
            pos = i / max(1, n - 1)
        stops.append((pos, color_str))

    return angle, stops


def _parse_radial_gradient(value: str, rect):
    """Parse a CSS radial-gradient() value.

    Returns (cx, cy, rx, ry, color_stops) or None.
    """
    import re
    m = re.match(r'radial-gradient\s*\((.+)\)\s*$', value, re.IGNORECASE | re.DOTALL)
    if not m:
        return None

    inner = m.group(1).strip()

    # Split by top-level commas
    parts = []
    depth = 0
    current = []
    for ch in inner:
        if ch == '(':
            depth += 1
            current.append(ch)
        elif ch == ')':
            depth -= 1
            current.append(ch)
        elif ch == ',' and depth == 0:
            parts.append(''.join(current).strip())
            current = []
        else:
            current.append(ch)
    if current:
        parts.append(''.join(current).strip())

    if not parts:
        return None

    # Default: ellipse at center
    cx = rect.x + rect.width / 2
    cy = rect.y + rect.height / 2
    rx = rect.width / 2
    ry = rect.height / 2
    start_idx = 0

    # Check if first part is a shape/size/position directive
    first = parts[0].strip().lower()
    if 'at ' in first or first.startswith('circle') or first.startswith('ellipse'):
        start_idx = 1
        # Parse "at X Y" position
        at_match = re.search(r'at\s+(.+)', first)
        if at_match:
            pos_str = at_match.group(1).strip()
            pos_parts = pos_str.split()
            from layout.text import _parse_px as _ppx
            try:
                p0 = pos_parts[0] if pos_parts else '50%'
                if p0.endswith('%'):
                    cx = rect.x + rect.width * float(p0[:-1]) / 100
                elif p0 in ('left',):
                    cx = rect.x
                elif p0 in ('right',):
                    cx = rect.x + rect.width
                elif p0 in ('center',):
                    pass
                else:
                    cx = rect.x + _ppx(p0)
            except Exception as _exc:
                _logger.debug("Ignored: %s", _exc)
            try:
                p1 = pos_parts[1] if len(pos_parts) > 1 else '50%'
                if p1.endswith('%'):
                    cy = rect.y + rect.height * float(p1[:-1]) / 100
                elif p1 in ('top',):
                    cy = rect.y
                elif p1 in ('bottom',):
                    cy = rect.y + rect.height
                elif p1 in ('center',):
                    pass
                else:
                    cy = rect.y + _ppx(p1)
            except Exception as _exc:
                _logger.debug("Ignored: %s", _exc)
        if 'circle' in first:
            rx = ry = min(rect.width, rect.height) / 2

    color_parts = parts[start_idx:]
    if not color_parts:
        return None

    stops = []
    n = len(color_parts)
    for i, part in enumerate(color_parts):
        part = part.strip()
        pos_match = re.search(r'\s+([\d.]+%|[\d.]+px)\s*$', part)
        if pos_match:
            color_str = part[:pos_match.start()].strip()
            pos_str = pos_match.group(1)
            if pos_str.endswith('%'):
                pos = float(pos_str[:-1]) / 100.0
            else:
                try:
                    from layout.text import _parse_px as _ppx
                    pos = _ppx(pos_str) / max(1.0, max(rx, ry))
                except Exception:
                    pos = i / max(1, n - 1)
        else:
            color_str = part
            pos = i / max(1, n - 1)
        stops.append((pos, color_str))

    return cx, cy, rx, ry, stops


def _get_list_marker(node, list_type: str) -> str:
    """Return the marker string for a list item."""
    from html.dom import Element

    if list_type == 'none':
        return ''
    if list_type == 'circle':
        return '\u25cb'   # ○
    if list_type == 'square':
        return '\u25a0'   # ■
    if list_type in ('decimal', 'decimal-leading-zero'):
        parent = getattr(node, 'parent', None)
        idx = 1
        if parent:
            try:
                idx = int(getattr(parent, 'attributes', {}).get('start', '1'))
            except Exception:
                idx = 1
            for sib in parent.children:
                if sib is node:
                    break
                if isinstance(sib, Element) and sib.tag == 'li':
                    idx += 1
        return f'{idx}.'
    if list_type in ('lower-alpha', 'lower-latin'):
        parent = getattr(node, 'parent', None)
        idx = 0
        if parent:
            for sib in parent.children:
                if sib is node:
                    break
                if isinstance(sib, Element) and sib.tag == 'li':
                    idx += 1
        return f'{chr(97 + idx % 26)}.'
    if list_type in ('upper-alpha', 'upper-latin'):
        parent = getattr(node, 'parent', None)
        idx = 0
        if parent:
            for sib in parent.children:
                if sib is node:
                    break
                if isinstance(sib, Element) and sib.tag == 'li':
                    idx += 1
        return f'{chr(65 + idx % 26)}.'
    if list_type in ('lower-roman',):
        parent = getattr(node, 'parent', None)
        idx = 1
        if parent:
            for sib in parent.children:
                if sib is node:
                    break
                if isinstance(sib, Element) and sib.tag == 'li':
                    idx += 1
        return _to_roman(idx).lower() + '.'
    if list_type in ('upper-roman',):
        parent = getattr(node, 'parent', None)
        idx = 1
        if parent:
            for sib in parent.children:
                if sib is node:
                    break
                if isinstance(sib, Element) and sib.tag == 'li':
                    idx += 1
        return _to_roman(idx) + '.'
    # disc (default)
    return '\u2022'   # •


def _to_roman(n: int) -> str:
    """Convert integer to uppercase Roman numeral string."""
    result = ''
    for value, numeral in (
        (1000, 'M'), (900, 'CM'), (500, 'D'), (400, 'CD'),
        (100, 'C'), (90, 'XC'), (50, 'L'), (40, 'XL'),
        (10, 'X'), (9, 'IX'), (5, 'V'), (4, 'IV'), (1, 'I'),
    ):
        while n >= value:
            result += numeral
            n -= value
    return result or 'I'


def _build_display_list(node, display_list: 'DisplayList', stacking_top: list) -> None:
    """Recursively build display list from laid-out DOM."""
    from html.dom import Element, Text
    from rendering.display_list import (
        DrawRect, DrawText, DrawBorder, DrawImage,
        PushClip, PopClip, DrawLinearGradient,
    )

    if isinstance(node, Element):
        style = node.style if hasattr(node, 'style') else {}
        display = style.get('display', 'block')
        position = style.get('position', 'static')

        # Skip hidden elements entirely (don't recurse into children either)
        if display == 'none':
            return

        if not hasattr(node, 'box') or node.box is None:
            # Element has no layout box but is visible — recurse into children
            for child in node.children:
                _build_display_list(child, display_list, stacking_top)
            return

        box = node.box

        # Elements with position: fixed or absolute go on top (stacking context)
        is_stacking = position in ('fixed', 'absolute')

        if is_stacking:
            target: list = []
        else:
            target = None  # use display_list directly

        def _emit(cmd):
            if target is not None:
                target.append(cmd)
            else:
                display_list.add(cmd)

        # Opacity
        opacity_str = style.get('opacity', '1')
        try:
            opacity = float(opacity_str)
        except (ValueError, TypeError):
            opacity = 1.0
        if opacity < 1.0:
            _emit(PushOpacity(opacity))

        # position: relative — apply visual offset via transform
        needs_pop_transform = False
        if position == 'relative':
            from layout.text import _parse_px as _ppx
            top_str = style.get('top', 'auto')
            left_str = style.get('left', 'auto')
            right_str = style.get('right', 'auto')
            bottom_str = style.get('bottom', 'auto')
            dx, dy = 0.0, 0.0
            if top_str not in ('auto', '', 'none'):
                try:
                    dy = _ppx(top_str)
                except Exception as _exc:
                    _logger.debug("Ignored: %s", _exc)
            elif bottom_str not in ('auto', '', 'none'):
                try:
                    dy = -_ppx(bottom_str)
                except Exception as _exc:
                    _logger.debug("Ignored: %s", _exc)
            if left_str not in ('auto', '', 'none'):
                try:
                    dx = _ppx(left_str)
                except Exception as _exc:
                    _logger.debug("Ignored: %s", _exc)
            elif right_str not in ('auto', '', 'none'):
                try:
                    dx = -_ppx(right_str)
                except Exception as _exc:
                    _logger.debug("Ignored: %s", _exc)
            if dx != 0.0 or dy != 0.0:
                _emit(PushTransform(dx=dx, dy=dy))
                needs_pop_transform = True

        # CSS transform property
        transform_str = style.get('transform', 'none')
        if transform_str and transform_str not in ('none', ''):
            t = _parse_css_transform(transform_str)
            if t:
                tdx, tdy, rot, sx, sy = t
                origin = style.get('transform-origin', '50% 50%')
                ox, oy = _parse_transform_origin(origin, box)
                _emit(PushTransform(dx=tdx, dy=tdy, rotate_deg=rot,
                                    scale_x=sx, scale_y=sy,
                                    origin_x=ox, origin_y=oy))
                needs_pop_transform = True

        from layout.text import _parse_px
        border_radius_str = style.get('border-radius', '0')
        radius = _parse_px(border_radius_str) if border_radius_str not in ('0', '') else 0

        # box-shadow — drawn first (behind background); supports multiple shadows
        shadow = style.get('box-shadow', 'none')
        if shadow and shadow not in ('none', ''):
            shadows = _parse_all_shadows(shadow)
            # CSS spec: first shadow is topmost, so draw in reverse order
            for ox, oy, blur, spread, scolor, inset in reversed(shadows):
                if not inset:  # inset shadows drawn after background
                    _emit(DrawBoxShadow(box.border_rect, ox, oy, blur, spread,
                                        scolor, border_radius=radius))

        # Background color
        bg = style.get('background-color', 'transparent')
        if bg and bg != 'transparent':
            _emit(DrawRect(box.border_rect, bg, border_radius=radius))

        node_bg_image = getattr(node, 'background_image', None)
        if node_bg_image is not None:
            _emit(DrawImage(
                box.border_rect.x, box.border_rect.y,
                node_bg_image, box.border_rect.width, box.border_rect.height,
            ))

        # <img> element: draw the image directly using the layout box
        if node.tag == 'img':
            image = getattr(node, 'image', None)
            if image is not None:
                from rendering.display_list import DrawImage
                _emit(DrawImage(box.x, box.y, image, box.content_width, box.content_height))

        # Background image (linear-gradient / radial-gradient)
        bg_image = style.get('background-image', 'none')
        if bg_image and bg_image not in ('none', ''):
            grad = _parse_linear_gradient(bg_image, box.border_rect)
            if grad is not None:
                angle, stops = grad
                _emit(DrawLinearGradient(box.border_rect, angle, stops))
            else:
                rgrad = _parse_radial_gradient(bg_image, box.border_rect)
                if rgrad is not None:
                    rcx, rcy, rrx, rry, rstops = rgrad
                    _emit(DrawRadialGradient(box.border_rect, rcx, rcy, rrx, rry, rstops))

        # Border
        _SIDES = ('top', 'right', 'bottom', 'left')
        side_styles = tuple(
            style.get(f'border-{s}-style', style.get('border-style', 'none'))
            for s in _SIDES
        )
        _element_color = style.get('color', 'black')
        side_colors = tuple(
            (lambda c: _element_color if c == 'currentcolor' else c)(
                style.get(f'border-{s}-color', style.get('border-color', _element_color))
            )
            for s in _SIDES
        )
        if any(s not in ('none', '', 'hidden') for s in side_styles):
            _emit(DrawBorder(box.border_rect, box.border, side_colors, side_styles))

        # Outline (drawn outside border box, does not affect layout)
        outline_style = style.get('outline-style', 'none')
        if outline_style and outline_style not in ('none', '', 'hidden'):
            outline_width = _parse_px(style.get('outline-width', '0'))
            if outline_width > 0:
                outline_color = style.get('outline-color', style.get('color', 'black'))
                outline_offset = _parse_px(style.get('outline-offset', '0'))
                _emit(DrawOutline(box.border_rect, outline_width, outline_style,
                                  outline_color, outline_offset))

        # overflow: hidden/scroll/auto → clip children to padding box
        overflow = style.get('overflow', style.get('overflow-x', 'visible'))
        needs_clip = overflow in ('hidden', 'scroll', 'auto')
        if needs_clip:
            _emit(PushClip(box.padding_rect))

        # List item marker (drawn before content)
        if display == 'list-item':
            list_type = style.get('list-style-type', '')
            if not list_type:
                parent = getattr(node, 'parent', None)
                if parent and hasattr(parent, 'tag'):
                    list_type = 'decimal' if parent.tag == 'ol' else 'disc'
                else:
                    list_type = 'disc'
            if list_type != 'none':
                marker = _get_list_marker(node, list_type)
                if marker:
                    font_family = style.get('font-family', 'Arial')
                    try:
                        font_size = int(_parse_px(style.get('font-size', '16px')))
                    except Exception:
                        font_size = 16
                    marker_x = box.x - font_size * 1.4
                    marker_y = box.y
                    _emit(DrawText(
                        marker_x, marker_y, marker,
                        (font_family, font_size, 'normal', ''),
                        style.get('color', 'black'),
                    ))

        # Children
        if is_stacking:
            child_dl = DisplayList()
            child_stacking: list = []
            for child in node.children:
                _build_display_list(child, child_dl, child_stacking)
            for cmd in child_dl:
                target.append(cmd)
            # Flatten child stacking contexts into our target
            for item in child_stacking:
                if isinstance(item, tuple) and len(item) == 3:
                    target.extend(item[2])
                else:
                    target.append(item)
        else:
            for child in node.children:
                _build_display_list(child, display_list, stacking_top)

        # Inline text/images (from line boxes if set)
        text_shadow = style.get('text-shadow', '')
        if hasattr(node, 'line_boxes'):
            for line in node.line_boxes:
                for item in line.items:
                    if getattr(item, 'image', None) is not None:
                        cmd = DrawImage(
                            item.x, item.y, item.image,
                            item.width, item.height,
                        )
                    elif getattr(item, 'control_type', '') == 'text-input':
                        cmd = DrawInput(
                            item.x, item.y, item.width, item.height,
                            item.control_value,
                            (item.font_family, int(item.font_size),
                             item.font_weight, 'italic' if item.font_italic else ''),
                            item.color,
                            background_color=item.background_color,
                            border_color=item.border_color,
                            border_width=item.border_width,
                        )
                    elif item.text:
                        cmd = DrawText(
                            item.x, item.y, item.text,
                            (item.font_family, int(item.font_size),
                             item.font_weight, 'italic' if item.font_italic else ''),
                            item.color,
                            decoration=item.decoration,
                            weight=item.font_weight,
                            italic=item.font_italic,
                            advance_width=item.width,
                            text_shadow=text_shadow if text_shadow and text_shadow != 'none' else '',
                        )
                    else:
                        continue
                    _emit(cmd)

        if needs_clip:
            _emit(PopClip())

        if opacity < 1.0:
            _emit(PopOpacity())

        if needs_pop_transform:
            _emit(PopTransform())

        if is_stacking:
            # Pack z-index for proper stacking order
            z_str = style.get('z-index', 'auto')
            try:
                z_index = int(z_str) if z_str not in ('auto', '') else 0
            except (ValueError, TypeError):
                z_index = 0
            stacking_top.append((z_index, len(stacking_top), target))

    elif isinstance(node, Text):
        pass  # Text is handled via line_boxes on parent elements

    else:
        for child in node.children:
            _build_display_list(child, display_list, stacking_top)


# ---------------------------------------------------------------------------
# Link extraction
# ---------------------------------------------------------------------------

def _extract_links(document, base_url: str = '') -> list:
    """Walk the DOM and collect (Rect, url) for all clickable <a href> elements."""
    from html.dom import Element
    links = []
    _collect_links(document, base_url, links)
    return links


def _collect_links(node, base_url: str, links: list) -> None:
    from html.dom import Element
    from network.http import resolve_url
    if not isinstance(node, Element):
        for child in node.children:
            _collect_links(child, base_url, links)
        return

    # Collect from inline line_boxes
    if hasattr(node, 'line_boxes') and node.line_boxes:
        link_map = {}   # origin_node -> [items]
        for line in node.line_boxes:
            for item in line.items:
                ln = getattr(item, 'origin_node', None)
                if ln is not None:
                    link_map.setdefault(ln, []).append(item)
        for a_node, items in link_map.items():
            href = a_node.attributes.get('href', '').strip()
            if not href or href.startswith('#'):
                continue
            try:
                url = resolve_url(base_url, href) if base_url else href
            except Exception:
                url = href
            if items:
                x1 = min(it.x for it in items)
                y1 = min(it.y for it in items)
                x2 = max(it.x + it.width for it in items)
                y2 = max(it.y + it.height for it in items)
                from layout.box import Rect
                links.append((Rect(x1, y1, x2 - x1, y2 - y1), url))

    # Block-level <a> with its own box
    if node.tag == 'a' and hasattr(node, 'box') and node.box is not None:
        href = node.attributes.get('href', '').strip()
        if href and not href.startswith('#'):
            try:
                url = resolve_url(base_url, href) if base_url else href
            except Exception:
                url = href
            links.append((node.box.border_rect, url))

    for child in node.children:
        _collect_links(child, base_url, links)
