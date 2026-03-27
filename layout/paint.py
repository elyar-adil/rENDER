"""CSS visual-effect parsing and display-list painting for rENDER browser engine."""
import logging
import re

_logger = logging.getLogger(__name__)

from css.utils import split_paren_aware as _split_top_level


# ---------------------------------------------------------------------------
# Shadow parsing
# ---------------------------------------------------------------------------

def _parse_single_shadow(s: str):
    """Parse one box-shadow value. Returns (ox, oy, blur, spread, color, inset) or None."""
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
        offsets = []
        for p in s.split():
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
    """Return the first non-inset shadow as (ox, oy, blur, spread, color), or None."""
    for part in _split_top_level(shadow_str):
        result = _parse_single_shadow(part)
        if result is not None:
            ox, oy, blur, spread, color, inset = result
            if not inset:
                return ox, oy, blur, spread, color
    return None


def _parse_all_shadows(shadow_str: str) -> list:
    """Return all shadows as list of (ox, oy, blur, spread, color, inset)."""
    results = []
    for part in _split_top_level(shadow_str):
        result = _parse_single_shadow(part)
        if result is not None:
            results.append(result)
    return results


# ---------------------------------------------------------------------------
# Transform parsing
# ---------------------------------------------------------------------------

def _parse_css_transform(transform_str: str):
    """Parse CSS transform. Returns (dx, dy, rotate_deg, scale_x, scale_y) or None."""
    from layout.text import _parse_px
    dx, dy, rot, sx, sy = 0.0, 0.0, 0.0, 1.0, 1.0
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
                dx += _parse_px(args[0]); found = True
            elif fn == 'translatey':
                dy += _parse_px(args[0]); found = True
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
                sx *= float(args[0]); found = True
            elif fn == 'scaley':
                sy *= float(args[0]); found = True
        except Exception as exc:
            _logger.debug('_parse_css_transform: %s', exc)
    return (dx, dy, rot, sx, sy) if found else None


def _parse_transform_origin(origin_str: str, box):
    """Parse transform-origin, return (ox, oy) in absolute coordinates."""
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
        else:
            ox = box.x + _parse_px(p0)
    except Exception as exc:
        _logger.debug('_parse_transform_origin x: %s', exc)
    try:
        p1 = parts[1] if len(parts) > 1 else '50%'
        if p1.endswith('%'):
            oy = box.y + box.content_height * float(p1[:-1]) / 100
        elif p1 == 'top':
            oy = box.y
        elif p1 == 'bottom':
            oy = box.y + box.content_height
        else:
            oy = box.y + _parse_px(p1)
    except Exception as exc:
        _logger.debug('_parse_transform_origin y: %s', exc)
    return ox, oy


# ---------------------------------------------------------------------------
# Gradient parsing
# ---------------------------------------------------------------------------

def _parse_linear_gradient(value: str, rect):
    """Parse linear-gradient(). Returns (angle_degrees, color_stops) or None."""
    m = re.match(r'linear-gradient\s*\((.+)\)\s*$', value, re.IGNORECASE | re.DOTALL)
    if not m:
        return None
    parts = _split_by_comma(m.group(1).strip())
    if not parts:
        return None

    angle = 180.0
    start_idx = 0
    first = parts[0].strip().lower()
    if first.endswith('deg'):
        try:
            angle = float(first[:-3]); start_idx = 1
        except ValueError:
            pass
    elif first.startswith('to '):
        angle = {'bottom': 180.0, 'top': 0.0, 'right': 90.0, 'left': 270.0,
                 'bottom right': 135.0, 'bottom left': 225.0,
                 'top right': 45.0, 'top left': 315.0}.get(first[3:].strip(), 180.0)
        start_idx = 1
    elif first.endswith('turn'):
        try:
            angle = float(first[:-4]) * 360; start_idx = 1
        except ValueError:
            pass
    elif first.endswith('rad'):
        try:
            import math as _m
            angle = _m.degrees(float(first[:-3])); start_idx = 1
        except ValueError:
            pass

    return angle, _parse_color_stops(parts[start_idx:], rect.width)


def _parse_radial_gradient(value: str, rect):
    """Parse radial-gradient(). Returns (cx, cy, rx, ry, color_stops) or None."""
    m = re.match(r'radial-gradient\s*\((.+)\)\s*$', value, re.IGNORECASE | re.DOTALL)
    if not m:
        return None
    parts = _split_by_comma(m.group(1).strip())
    if not parts:
        return None

    cx = rect.x + rect.width / 2
    cy = rect.y + rect.height / 2
    rx = rect.width / 2
    ry = rect.height / 2
    start_idx = 0

    first = parts[0].strip().lower()
    if 'at ' in first or first.startswith('circle') or first.startswith('ellipse'):
        start_idx = 1
        at_m = re.search(r'at\s+(.+)', first)
        if at_m:
            cx, cy = _parse_radial_position(at_m.group(1).strip(), rect)
        if 'circle' in first:
            rx = ry = min(rect.width, rect.height) / 2

    stops = _parse_color_stops(parts[start_idx:], max(rx, ry))
    return cx, cy, rx, ry, stops


def _parse_radial_position(pos_str, rect):
    from layout.text import _parse_px as _ppx
    parts = pos_str.split()
    cx = rect.x + rect.width / 2
    cy = rect.y + rect.height / 2
    try:
        p0 = parts[0] if parts else '50%'
        if p0.endswith('%'):
            cx = rect.x + rect.width * float(p0[:-1]) / 100
        elif p0 == 'left':
            cx = rect.x
        elif p0 == 'right':
            cx = rect.x + rect.width
        elif p0 != 'center':
            cx = rect.x + _ppx(p0)
    except Exception as exc:
        _logger.debug('_parse_radial_position x: %s', exc)
    try:
        p1 = parts[1] if len(parts) > 1 else '50%'
        if p1.endswith('%'):
            cy = rect.y + rect.height * float(p1[:-1]) / 100
        elif p1 == 'top':
            cy = rect.y
        elif p1 == 'bottom':
            cy = rect.y + rect.height
        elif p1 != 'center':
            cy = rect.y + _ppx(p1)
    except Exception as exc:
        _logger.debug('_parse_radial_position y: %s', exc)
    return cx, cy


def _split_by_comma(inner: str) -> list:
    """Split CSS value by top-level commas (respecting parentheses)."""
    parts, current, depth = [], [], 0
    for ch in inner:
        if ch == '(':
            depth += 1; current.append(ch)
        elif ch == ')':
            depth -= 1; current.append(ch)
        elif ch == ',' and depth == 0:
            parts.append(''.join(current).strip()); current = []
        else:
            current.append(ch)
    if current:
        parts.append(''.join(current).strip())
    return parts


def _parse_color_stops(color_parts, extent) -> list:
    """Parse color stop strings into [(position 0..1, color_str), ...]."""
    from layout.text import _parse_px as _ppx
    stops = []
    n = len(color_parts)
    for i, part in enumerate(color_parts):
        part = part.strip()
        pos_m = re.search(r'\s+([\d.]+%|[\d.]+px)\s*$', part)
        if pos_m:
            color_str = part[:pos_m.start()].strip()
            ps = pos_m.group(1)
            if ps.endswith('%'):
                pos = float(ps[:-1]) / 100.0
            else:
                try:
                    pos = _ppx(ps) / max(1.0, extent)
                except Exception:
                    pos = i / max(1, n - 1)
        else:
            color_str = part
            pos = i / max(1, n - 1)
        stops.append((pos, color_str))
    return stops


# ---------------------------------------------------------------------------
# List markers
# ---------------------------------------------------------------------------

def _get_list_marker(node, list_type: str) -> str:
    from html.dom import Element
    if list_type == 'none':
        return ''
    if list_type == 'circle':
        return '\u25cb'
    if list_type == 'square':
        return '\u25a0'
    if list_type in ('decimal', 'decimal-leading-zero'):
        idx = _sibling_index(node, 1)
        return f'{idx}.'
    if list_type in ('lower-alpha', 'lower-latin'):
        return f'{chr(97 + _sibling_index(node, 0) % 26)}.'
    if list_type in ('upper-alpha', 'upper-latin'):
        return f'{chr(65 + _sibling_index(node, 0) % 26)}.'
    if list_type == 'lower-roman':
        return _to_roman(_sibling_index(node, 1)).lower() + '.'
    if list_type == 'upper-roman':
        return _to_roman(_sibling_index(node, 1)) + '.'
    return '\u2022'   # disc default


def _sibling_index(node, start: int) -> int:
    from html.dom import Element
    parent = getattr(node, 'parent', None)
    idx = start
    if parent:
        for sib in parent.children:
            if sib is node:
                break
            if isinstance(sib, Element) and sib.tag == 'li':
                idx += 1
    return idx


def _to_roman(n: int) -> str:
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


# ---------------------------------------------------------------------------
# Display-list builder
# ---------------------------------------------------------------------------

def build_display_list(node, display_list, stacking_top: list) -> None:
    """Recursively paint laid-out DOM nodes into *display_list*."""
    from html.dom import Element, Text
    from rendering.display_list import (
        DisplayList, DrawRect, DrawText, DrawBorder, DrawImage,
        PushClip, PopClip, DrawLinearGradient,
        PushOpacity, PopOpacity, PushTransform, PopTransform,
        DrawBoxShadow, DrawInput, DrawOutline, DrawRadialGradient,
    )
    from layout.text import _parse_px

    if isinstance(node, Element):
        style = node.style if hasattr(node, 'style') else {}
        display = style.get('display', 'block')
        position = style.get('position', 'static')

        if display == 'none':
            return

        if not hasattr(node, 'box') or node.box is None:
            for child in node.children:
                build_display_list(child, display_list, stacking_top)
            return

        box = node.box
        is_stacking = position in ('fixed', 'absolute')
        target: list = [] if is_stacking else None

        def _emit(cmd):
            if target is not None:
                target.append(cmd)
            else:
                display_list.add(cmd)

        # Opacity
        opacity = 1.0
        try:
            opacity = float(style.get('opacity', '1'))
        except (ValueError, TypeError):
            pass
        if opacity < 1.0:
            _emit(PushOpacity(opacity))

        # position: relative visual offset
        needs_pop_transform = False
        if position == 'relative':
            dx, dy = 0.0, 0.0
            for css_prop, axis, sign in (('top', 'dy', 1), ('bottom', 'dy', -1),
                                          ('left', 'dx', 1), ('right', 'dx', -1)):
                val_str = style.get(css_prop, 'auto')
                if val_str not in ('auto', '', 'none'):
                    try:
                        delta = _parse_px(val_str) * sign
                        if axis == 'dx':
                            dx += delta
                        else:
                            dy += delta
                    except Exception as exc:
                        _logger.debug('relative offset: %s', exc)
                    break  # only first of top/bottom or left/right counts
            if dx != 0.0 or dy != 0.0:
                _emit(PushTransform(dx=dx, dy=dy))
                needs_pop_transform = True

        # CSS transform property
        transform_str = style.get('transform', 'none')
        if transform_str and transform_str not in ('none', ''):
            t = _parse_css_transform(transform_str)
            if t:
                tdx, tdy, rot, sx, sy = t
                ox, oy = _parse_transform_origin(style.get('transform-origin', '50% 50%'), box)
                _emit(PushTransform(dx=tdx, dy=tdy, rotate_deg=rot,
                                    scale_x=sx, scale_y=sy, origin_x=ox, origin_y=oy))
                needs_pop_transform = True

        radius = 0
        try:
            r_str = style.get('border-radius', '0')
            if r_str not in ('0', ''):
                radius = _parse_px(r_str)
        except Exception:
            pass

        # box-shadow (outer, drawn behind background)
        shadow = style.get('box-shadow', 'none')
        if shadow and shadow not in ('none', ''):
            for ox, oy, blur, spread, scolor, inset in reversed(_parse_all_shadows(shadow)):
                if not inset:
                    _emit(DrawBoxShadow(box.border_rect, ox, oy, blur, spread,
                                        scolor, border_radius=radius))

        # Background colour
        bg = style.get('background-color', 'transparent')
        if bg and bg != 'transparent':
            _emit(DrawRect(box.border_rect, bg, border_radius=radius))

        # Pre-fetched background image (raster)
        node_bg_image = getattr(node, 'background_image', None)
        if node_bg_image is not None:
            _emit(DrawImage(box.border_rect.x, box.border_rect.y,
                            node_bg_image, box.border_rect.width, box.border_rect.height))

        # <img> tag
        if node.tag == 'img':
            image = getattr(node, 'image', None)
            if image is not None:
                _emit(DrawImage(box.x, box.y, image, box.content_width, box.content_height))

        # CSS background-image (gradient)
        bg_image = style.get('background-image', 'none')
        if bg_image and bg_image not in ('none', ''):
            grad = _parse_linear_gradient(bg_image, box.border_rect)
            if grad is not None:
                _emit(DrawLinearGradient(box.border_rect, grad[0], grad[1]))
            else:
                rgrad = _parse_radial_gradient(bg_image, box.border_rect)
                if rgrad is not None:
                    rcx, rcy, rrx, rry, rstops = rgrad
                    _emit(DrawRadialGradient(box.border_rect, rcx, rcy, rrx, rry, rstops))

        # Border
        _SIDES = ('top', 'right', 'bottom', 'left')
        el_color = style.get('color', 'black')
        side_styles = tuple(
            style.get(f'border-{s}-style', style.get('border-style', 'none'))
            for s in _SIDES)
        side_colors = tuple(
            (lambda c: el_color if c == 'currentcolor' else c)(
                style.get(f'border-{s}-color', style.get('border-color', el_color)))
            for s in _SIDES)
        if any(s not in ('none', '', 'hidden') for s in side_styles):
            _emit(DrawBorder(box.border_rect, box.border, side_colors, side_styles))

        # Outline
        outline_style = style.get('outline-style', 'none')
        if outline_style and outline_style not in ('none', '', 'hidden'):
            outline_width = _parse_px(style.get('outline-width', '0'))
            if outline_width > 0:
                _emit(DrawOutline(box.border_rect, outline_width, outline_style,
                                  style.get('outline-color', el_color),
                                  _parse_px(style.get('outline-offset', '0'))))

        # overflow clip
        overflow = style.get('overflow', style.get('overflow-x', 'visible'))
        needs_clip = overflow in ('hidden', 'scroll', 'auto')
        if needs_clip:
            _emit(PushClip(box.padding_rect))

        # List item marker
        if display == 'list-item':
            list_type = style.get('list-style-type', '')
            if not list_type:
                parent = getattr(node, 'parent', None)
                list_type = ('decimal' if parent and hasattr(parent, 'tag')
                             and parent.tag == 'ol' else 'disc')
            if list_type != 'none':
                marker = _get_list_marker(node, list_type)
                if marker:
                    font_family = style.get('font-family', 'Arial')
                    try:
                        font_size = int(_parse_px(style.get('font-size', '16px')))
                    except Exception:
                        font_size = 16
                    _emit(DrawText(box.x - font_size * 1.4, box.y, marker,
                                  (font_family, font_size, 'normal', ''),
                                  style.get('color', 'black')))

        # Children
        text_shadow = style.get('text-shadow', '')
        if is_stacking:
            child_dl = DisplayList()
            child_stacking: list = []
            for child in node.children:
                build_display_list(child, child_dl, child_stacking)
            for cmd in child_dl:
                target.append(cmd)
            for item in child_stacking:
                if isinstance(item, tuple) and len(item) == 3:
                    target.extend(item[2])
                else:
                    target.append(item)
        else:
            for child in node.children:
                build_display_list(child, display_list, stacking_top)

        # Inline line boxes (text, images, inputs)
        if hasattr(node, 'line_boxes'):
            for line in node.line_boxes:
                for item in line.items:
                    if getattr(item, 'image', None) is not None:
                        cmd = DrawImage(item.x, item.y, item.image, item.width, item.height)
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
            z_str = style.get('z-index', 'auto')
            try:
                z_index = int(z_str) if z_str not in ('auto', '') else 0
            except (ValueError, TypeError):
                z_index = 0
            stacking_top.append((z_index, len(stacking_top), target))

    elif isinstance(node, Text):
        pass   # handled via line_boxes on parent

    else:
        for child in node.children:
            build_display_list(child, display_list, stacking_top)
