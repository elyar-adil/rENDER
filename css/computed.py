"""Resolve CSS computed values: convert relative units to absolute px values."""
import re
from html.dom import Document, Element


DEFAULT_FONT_SIZE = 16  # browser default px


def compute(document: Document, viewport_width: int = 980, viewport_height: int = 600) -> None:
    """Convert all element style values to computed values in-place (iterative)."""
    root_font_size = DEFAULT_FONT_SIZE
    # Iterative top-down walk so we can propagate font-size parent→child
    stack = [(document, DEFAULT_FONT_SIZE)]
    while stack:
        node, parent_font_size = stack.pop()
        if isinstance(node, Element):
            font_size = _process_element(node, parent_font_size, root_font_size, viewport_width, viewport_height)
        else:
            font_size = parent_font_size
        # Push children in reverse (so left-to-right processing)
        for child in reversed(node.children):
            stack.append((child, font_size))


def _process_element(node: Element, parent_font_size: float, root_font_size: float, vw: int, vh: int) -> float:
    """Resolve units on one element, return its resolved font-size."""
    fs_str = node.style.get('font-size', f'{parent_font_size}px')
    font_size = _resolve_length(fs_str, parent_font_size, root_font_size, vw, vh, is_font_size=True)
    if font_size is None:
        font_size = parent_font_size
    node.style['font-size'] = f'{int(font_size)}px'

    _length_props = [
        'width', 'height', 'min-width', 'min-height', 'max-width', 'max-height',
        'margin-top', 'margin-right', 'margin-bottom', 'margin-left',
        'padding-top', 'padding-right', 'padding-bottom', 'padding-left',
        'border-top-width', 'border-right-width', 'border-bottom-width', 'border-left-width',
        'top', 'right', 'bottom', 'left',
        'word-spacing', 'letter-spacing',
        'border-radius', 'text-indent',
        'outline-width', 'outline-offset',
        'column-gap', 'row-gap',
        'border-top-left-radius', 'border-top-right-radius',
        'border-bottom-left-radius', 'border-bottom-right-radius',
    ]
    for prop in _length_props:
        if prop in node.style:
            resolved = _resolve_length(node.style[prop], font_size, root_font_size, vw, vh)
            if resolved is not None:
                node.style[prop] = f'{resolved}px'

    # line-height: resolve em/rem but keep unitless/normal as-is
    lh = node.style.get('line-height', '')
    if lh and lh not in ('normal', 'inherit', 'initial', ''):
        resolved = _resolve_length(lh, font_size, root_font_size, vw, vh)
        if resolved is not None:
            node.style['line-height'] = f'{resolved}px'

    return font_size




def _resolve_length(value: str, parent_font_size: float, root_font_size: float,
                    vw: int, vh: int, is_font_size: bool = False):
    """Resolve a CSS length value to px. Returns None if not resolvable (e.g. 'auto', '%', keyword)."""
    if not value or value in ('auto', 'none', 'normal', 'inherit', 'initial'):
        return None

    value = value.strip()

    # Named font sizes
    if is_font_size:
        named = {'xx-small': 9, 'x-small': 10, 'small': 13, 'medium': 16,
                 'large': 18, 'x-large': 24, 'xx-large': 32}
        if value in named:
            return float(named[value])
        relative = {'smaller': 0.8, 'larger': 1.25}
        if value in relative:
            return (parent_font_size or 16) * relative[value]

    # Numeric with unit
    m = re.fullmatch(r'([+-]?[\d.]+)\s*(px|em|rem|vw|vh|%|pt|pc|cm|mm|in|ex|ch|)', value)
    if m:
        num = float(m.group(1))
        unit = m.group(2)
        if unit == 'px' or unit == '':
            return num
        elif unit == 'em':
            return num * (parent_font_size or 16)
        elif unit == 'rem':
            return num * root_font_size
        elif unit == 'vw':
            return num * vw / 100
        elif unit == 'vh':
            return num * vh / 100
        elif unit == '%' and is_font_size:
            return num * (parent_font_size or 16) / 100
        elif unit == 'pt':
            return num * 96 / 72
        elif unit == 'pc':
            return num * 96 / 6
        elif unit == 'cm':
            return num * 96 / 2.54
        elif unit == 'mm':
            return num * 96 / 25.4
        elif unit == 'in':
            return num * 96
        else:
            return None  # % without context

    return None
