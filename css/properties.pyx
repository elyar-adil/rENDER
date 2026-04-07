"""CSS property metadata: initial values and inheritance flags."""


import re
from dataclasses import dataclass
from typing import Optional


@dataclass
class PropertyDef:
    initial: str          # initial value as string
    inherited: bool       # whether property is inherited


# The property registry - dict mapping property name to PropertyDef
PROPERTIES: dict = {
    # Box model
    'display':          PropertyDef('inline', False),
    'visibility':       PropertyDef('visible', True),
    'overflow':         PropertyDef('visible', False),
    'overflow-x':       PropertyDef('visible', False),
    'overflow-y':       PropertyDef('visible', False),
    'box-sizing':       PropertyDef('content-box', False),

    # Dimensions
    'width':            PropertyDef('auto', False),
    'height':           PropertyDef('auto', False),
    'min-width':        PropertyDef('0', False),
    'min-height':       PropertyDef('0', False),
    'max-width':        PropertyDef('none', False),
    'max-height':       PropertyDef('none', False),

    # Margin (shorthand + individual)
    'margin':           PropertyDef('0', False),
    'margin-top':       PropertyDef('0', False),
    'margin-right':     PropertyDef('0', False),
    'margin-bottom':    PropertyDef('0', False),
    'margin-left':      PropertyDef('0', False),

    # Padding
    'padding':          PropertyDef('0', False),
    'padding-top':      PropertyDef('0', False),
    'padding-right':    PropertyDef('0', False),
    'padding-bottom':   PropertyDef('0', False),
    'padding-left':     PropertyDef('0', False),

    # Border
    'border':               PropertyDef('none', False),
    'border-top':           PropertyDef('none', False),
    'border-right':         PropertyDef('none', False),
    'border-bottom':        PropertyDef('none', False),
    'border-left':          PropertyDef('none', False),
    'border-width':         PropertyDef('medium', False),
    'border-top-width':     PropertyDef('medium', False),
    'border-right-width':   PropertyDef('medium', False),
    'border-bottom-width':  PropertyDef('medium', False),
    'border-left-width':    PropertyDef('medium', False),
    'border-style':         PropertyDef('none', False),
    'border-top-style':     PropertyDef('none', False),
    'border-right-style':   PropertyDef('none', False),
    'border-bottom-style':  PropertyDef('none', False),
    'border-left-style':    PropertyDef('none', False),
    'border-color':         PropertyDef('currentcolor', False),
    'border-top-color':     PropertyDef('currentcolor', False),
    'border-right-color':   PropertyDef('currentcolor', False),
    'border-bottom-color':  PropertyDef('currentcolor', False),
    'border-left-color':    PropertyDef('currentcolor', False),
    'border-radius':                PropertyDef('0', False),
    'border-top-left-radius':       PropertyDef('0', False),
    'border-top-right-radius':      PropertyDef('0', False),
    'border-bottom-right-radius':   PropertyDef('0', False),
    'border-bottom-left-radius':    PropertyDef('0', False),

    # Color & Background
    'color':                    PropertyDef('black', True),
    'background-color':         PropertyDef('transparent', False),
    'background-image':         PropertyDef('none', False),
    'background':               PropertyDef('transparent', False),
    'background-position':      PropertyDef('0% 0%', False),
    'background-size':          PropertyDef('auto', False),
    'background-repeat':        PropertyDef('repeat', False),
    'background-attachment':    PropertyDef('scroll', False),
    'background-clip':          PropertyDef('border-box', False),
    'background-origin':        PropertyDef('padding-box', False),
    'opacity':                  PropertyDef('1', False),

    # Font
    'font-family':  PropertyDef('Times', True),
    'font-size':    PropertyDef('16px', True),
    'font-weight':  PropertyDef('normal', True),
    'font-style':   PropertyDef('normal', True),
    'font':         PropertyDef('', True),

    # Text
    'text-align':       PropertyDef('left', True),
    'text-decoration':  PropertyDef('none', False),
    'text-decoration-line': PropertyDef('none', False),
    'text-transform':   PropertyDef('none', True),
    'line-height':      PropertyDef('normal', True),
    'word-spacing':     PropertyDef('0px', True),
    'letter-spacing':   PropertyDef('normal', True),
    'white-space':      PropertyDef('normal', True),
    'word-break':       PropertyDef('normal', True),
    'text-overflow':    PropertyDef('clip', False),
    'text-indent':      PropertyDef('0', True),

    # Positioning
    'float':    PropertyDef('none', False),
    'clear':    PropertyDef('none', False),
    'position': PropertyDef('static', False),
    'top':      PropertyDef('auto', False),
    'right':    PropertyDef('auto', False),
    'bottom':   PropertyDef('auto', False),
    'left':     PropertyDef('auto', False),
    'z-index':  PropertyDef('auto', False),

    # Flexbox
    'flex':             PropertyDef('0 1 auto', False),
    'flex-direction':   PropertyDef('row', False),
    'flex-wrap':        PropertyDef('nowrap', False),
    'flex-grow':        PropertyDef('0', False),
    'flex-shrink':      PropertyDef('1', False),
    'flex-basis':       PropertyDef('auto', False),
    'justify-content':  PropertyDef('flex-start', False),
    'align-items':      PropertyDef('stretch', False),
    'align-self':       PropertyDef('auto', False),
    'align-content':    PropertyDef('stretch', False),
    'flex-flow':        PropertyDef('row nowrap', False),
    'order':            PropertyDef('0', False),

    # Grid
    'grid-template-columns':    PropertyDef('none', False),
    'grid-template-rows':       PropertyDef('none', False),
    'grid-auto-columns':        PropertyDef('auto', False),
    'grid-auto-rows':           PropertyDef('auto', False),
    'grid-auto-flow':           PropertyDef('row', False),
    'grid-column':              PropertyDef('auto', False),
    'grid-row':                 PropertyDef('auto', False),
    'grid-column-start':        PropertyDef('auto', False),
    'grid-column-end':          PropertyDef('auto', False),
    'grid-row-start':           PropertyDef('auto', False),
    'grid-row-end':             PropertyDef('auto', False),
    'grid-column-gap':          PropertyDef('0', False),
    'grid-row-gap':             PropertyDef('0', False),
    'grid-gap':                 PropertyDef('0', False),
    'grid-area':                PropertyDef('auto', False),
    'grid-template-areas':      PropertyDef('none', False),

    # Gap (modern)
    'gap':          PropertyDef('0', False),
    'row-gap':      PropertyDef('0', False),
    'column-gap':   PropertyDef('0', False),

    # Visual effects
    'box-shadow':       PropertyDef('none', False),
    'text-shadow':      PropertyDef('none', False),
    'transform':        PropertyDef('none', False),
    'transform-origin': PropertyDef('50% 50%', False),
    'transition':       PropertyDef('none', False),
    'animation':        PropertyDef('none', False),
    'will-change':      PropertyDef('auto', False),

    # Outline
    'outline':          PropertyDef('none', False),
    'outline-width':    PropertyDef('medium', False),
    'outline-style':    PropertyDef('none', False),
    'outline-color':    PropertyDef('currentcolor', False),
    'outline-offset':   PropertyDef('0', False),

    # Other
    'cursor':           PropertyDef('auto', True),
    'pointer-events':   PropertyDef('auto', True),
    'content':          PropertyDef('normal', False),
    'list-style':           PropertyDef('disc outside none', True),
    'list-style-type':      PropertyDef('disc', True),
    'list-style-position':  PropertyDef('outside', True),
    'list-style-image':     PropertyDef('none', True),

    # Image / replaced content
    'object-fit':       PropertyDef('fill', False),
    'object-position':  PropertyDef('50% 50%', False),
    'aspect-ratio':     PropertyDef('auto', False),
    'image-rendering':  PropertyDef('auto', False),

    # Filters / effects
    'filter':           PropertyDef('none', False),
    'backdrop-filter':  PropertyDef('none', False),
    'clip-path':        PropertyDef('none', False),
    'mask':             PropertyDef('none', False),

    # Writing / text direction
    'writing-mode':     PropertyDef('horizontal-tb', True),
    'direction':        PropertyDef('ltr', True),
    'unicode-bidi':     PropertyDef('normal', False),

    # Containment
    'contain':          PropertyDef('none', False),
    'isolation':        PropertyDef('auto', False),

    # Counters / lists
    'counter-reset':        PropertyDef('none', False),
    'counter-increment':    PropertyDef('none', False),
    'counter-set':          PropertyDef('none', False),
    'quotes':               PropertyDef('auto', True),

    # Columns
    'column-count':     PropertyDef('auto', False),
    'column-width':     PropertyDef('auto', False),
    'column-gap':       PropertyDef('normal', False),
    'columns':          PropertyDef('auto auto', False),
    'column-rule':      PropertyDef('medium none currentcolor', False),
    'column-fill':      PropertyDef('balance', False),
    'column-span':      PropertyDef('none', False),

    # Font advanced
    'font-variant':             PropertyDef('normal', True),
    'font-feature-settings':    PropertyDef('normal', True),
    'font-kerning':             PropertyDef('auto', True),
    'font-stretch':             PropertyDef('normal', True),

    # Shape
    'shape-outside':    PropertyDef('none', False),
    'shape-margin':     PropertyDef('0', False),

    # Interaction
    'user-select':          PropertyDef('auto', False),
    'appearance':           PropertyDef('none', False),
    'resize':               PropertyDef('none', False),
    'touch-action':         PropertyDef('auto', False),

    # Scroll
    'scroll-behavior':      PropertyDef('auto', False),
    'overscroll-behavior':  PropertyDef('auto', False),
    'scroll-snap-type':     PropertyDef('none', False),
    'scroll-snap-align':    PropertyDef('none', False),
    'scroll-margin':        PropertyDef('0', False),
    'scroll-padding':       PropertyDef('auto', False),

    # Transitions / animations (longhands)
    'transition-property':          PropertyDef('all', False),
    'transition-duration':          PropertyDef('0s', False),
    'transition-timing-function':   PropertyDef('ease', False),
    'transition-delay':             PropertyDef('0s', False),
    'animation-name':               PropertyDef('none', False),
    'animation-duration':           PropertyDef('0s', False),
    'animation-timing-function':    PropertyDef('ease', False),
    'animation-delay':              PropertyDef('0s', False),
    'animation-iteration-count':    PropertyDef('1', False),
    'animation-direction':          PropertyDef('normal', False),
    'animation-fill-mode':          PropertyDef('none', False),
    'animation-play-state':         PropertyDef('running', False),

    # Grid (additional)
    'grid-template':        PropertyDef('none', False),
    'grid':                 PropertyDef('none', False),

    # Misc
    'speak':                PropertyDef('normal', True),
    'print-color-adjust':   PropertyDef('economy', False),
    'forced-color-adjust':  PropertyDef('auto', True),
    'accent-color':         PropertyDef('auto', False),
    'caret-color':          PropertyDef('auto', True),
    'color-scheme':         PropertyDef('normal', False),
}


# -----------------------------------------------------------------------
# Border value parsing helpers
# -----------------------------------------------------------------------

# Known border-style keywords
_BORDER_STYLES = {
    'none', 'hidden', 'dotted', 'dashed', 'solid', 'double',
    'groove', 'ridge', 'inset', 'outset',
}

# Known border-width keywords
_BORDER_WIDTH_KEYWORDS = {'thin', 'medium', 'thick'}

# Simple color keywords (non-exhaustive but covers the common ones)
_COLOR_KEYWORDS = {
    'transparent', 'currentcolor', 'black', 'white', 'red', 'green',
    'blue', 'yellow', 'orange', 'purple', 'pink', 'gray', 'grey',
    'brown', 'cyan', 'magenta', 'lime', 'maroon', 'navy', 'olive',
    'teal', 'silver', 'aqua', 'fuchsia', 'coral', 'salmon', 'tan',
    'gold', 'khaki', 'indigo', 'violet',
}


def _is_color_token(tok: str) -> bool:
    """Return True if tok looks like a CSS color value."""
    t = tok.lower()
    if t in _COLOR_KEYWORDS:
        return True
    if t.startswith('#'):
        return True
    if t.startswith('rgb(') or t.startswith('rgba(') or t.startswith('hsl(') or t.startswith('hsla('):
        return True
    return False


def _is_length_token(tok: str) -> bool:
    """Return True if tok looks like a CSS length/dimension."""
    t = tok.lower()
    if t in _BORDER_WIDTH_KEYWORDS:
        return True
    # e.g. 1px, 0.5em, 2rem, 0
    if re.match(r'^-?\d+(\.\d+)?(px|em|rem|vw|vh|vmin|vmax|pt|cm|mm|in|pc|ex|ch|%)?$', t):
        return True
    return False


def _parse_border_value(value: str) -> tuple:
    """Parse a border shorthand value into (width, style, color).

    Returns a 3-tuple of (width_str, style_str, color_str) where any
    component not found is None.
    Handles values like:
      "1px solid #ccc", "none", "2px dashed red", "solid"
    """
    # Tokenize respecting parens (for rgb(), etc.)
    tokens = []
    current = []
    depth = 0
    for ch in value:
        if ch == '(':
            depth += 1
            current.append(ch)
        elif ch == ')':
            depth -= 1
            current.append(ch)
        elif ch in (' ', '\t') and depth == 0:
            if current:
                tokens.append(''.join(current))
                current = []
        else:
            current.append(ch)
    if current:
        tokens.append(''.join(current))

    width = None
    style = None
    color = None

    for tok in tokens:
        tl = tok.lower()
        if tl == 'none':
            # "none" can mean no border — treat as style=none
            if style is None:
                style = tok
        elif tl in _BORDER_STYLES:
            style = tok
        elif _is_color_token(tl):
            color = tok
        elif _is_length_token(tl):
            width = tok

    return width, style, color


# -----------------------------------------------------------------------
# Font shorthand parsing
# -----------------------------------------------------------------------

def _parse_font_value(value: str) -> dict:
    """Parse a CSS font shorthand value.

    Handles forms like:
      "bold 14px/1.5 Arial, sans-serif"
      "italic 12px Times"
      "14px sans-serif"
      "bold italic 16px/1.2 Georgia, serif"
    Returns dict with any of: font-style, font-weight, font-size,
    line-height, font-family.
    """
    result = {}

    _FONT_STYLES = {'italic', 'oblique', 'normal'}
    _FONT_WEIGHTS = {'bold', 'bolder', 'lighter', 'normal',
                     '100', '200', '300', '400', '500', '600', '700', '800', '900'}
    _FONT_VARIANTS = {'small-caps', 'normal'}
    _SIZE_KEYWORDS = {
        'xx-small', 'x-small', 'small', 'medium', 'large',
        'x-large', 'xx-large', 'smaller', 'larger',
    }

    def _is_font_size(tok: str) -> bool:
        tl = tok.lower().split('/')[0]
        if tl in _SIZE_KEYWORDS:
            return True
        # In font shorthand, a bare number like "300" is a valid font-weight
        # and must not be mistaken for font-size.
        if tl == '0':
            return True
        if re.match(r'^-?\d+(\.\d+)?(px|em|rem|vw|vh|vmin|vmax|pt|pc|cm|mm|in|ex|ch|%)$', tl):
            return True
        return False

    # Split off font-family first: everything after first token that is a size
    # Strategy: scan left-to-right, collect pre-size tokens (style/weight/variant),
    # then find the size token, then the rest is family.
    parts = value.split(',')
    # Rejoin but split on the first part carefully
    # We need to handle: "bold 14px/1.5 Arial, sans-serif"
    # First token sequence before family may have spaces; family has commas.

    # Grab the portion before the first comma as the "preamble + first-family"
    first_part = parts[0].strip()
    rest_families = [p.strip() for p in parts[1:]]

    # Tokenize first_part by spaces
    tokens = first_part.split()

    font_style = None
    font_weight = None
    font_variant = None
    size_idx = None

    for i, tok in enumerate(tokens):
        tl = tok.lower()
        if _is_font_size(tl):
            size_idx = i
            break
        if tl in _FONT_STYLES and font_style is None:
            font_style = tok
        elif tl in _FONT_WEIGHTS and font_weight is None:
            font_weight = tok
        elif tl in _FONT_VARIANTS and font_variant is None:
            font_variant = tok

    if font_style:
        result['font-style'] = font_style
    if font_weight:
        result['font-weight'] = font_weight

    if size_idx is not None:
        size_tok = tokens[size_idx]
        if '/' in size_tok:
            sz, lh = size_tok.split('/', 1)
            result['font-size'] = sz
            result['line-height'] = lh
        else:
            result['font-size'] = size_tok

        # Everything after size_idx in first_part becomes first family token
        first_family_tokens = tokens[size_idx + 1:]
        first_family = ' '.join(first_family_tokens)
        all_families = []
        if first_family:
            all_families.append(first_family)
        all_families.extend(rest_families)
        if all_families:
            result['font-family'] = ', '.join(all_families)
    else:
        # No size found — might just be a family name or weight
        if rest_families:
            result['font-family'] = ', '.join([first_part] + rest_families)

    return result


# -----------------------------------------------------------------------
# Outline shorthand parsing
# -----------------------------------------------------------------------

_OUTLINE_STYLES = frozenset({
    'none', 'hidden', 'dotted', 'dashed', 'solid', 'double',
    'groove', 'ridge', 'inset', 'outset', 'auto',
})


def _parse_outline_value(value: str) -> dict:
    """Parse CSS outline shorthand: [width] [style] [color]."""
    result = {}
    parts = value.split()
    for part in parts:
        pl = part.lower()
        if pl in _OUTLINE_STYLES:
            result['outline-style'] = part
        elif re.match(r'^-?\d+(\.\d+)?(px|em|rem|pt)?$', pl) or pl in ('thin', 'medium', 'thick'):
            result['outline-width'] = part
        elif _is_color_token(pl):
            result['outline-color'] = part
    if not result:
        result['outline-style'] = value
    return result


# -----------------------------------------------------------------------
# Background shorthand parsing
# -----------------------------------------------------------------------

def _parse_background_value(value: str) -> dict:
    """Parse a CSS background shorthand value.

    Extracts background-color, background-image, background-repeat,
    background-position at minimum.
    """
    result = {}

    _BG_REPEATS = {'repeat', 'repeat-x', 'repeat-y', 'no-repeat', 'space', 'round'}
    _BG_ATTACHMENTS = {'scroll', 'fixed', 'local'}
    _BG_POSITIONS_KW = {'top', 'right', 'bottom', 'left', 'center'}

    # Tokenize (respecting parens for url() and color functions)
    tokens = []
    current = []
    depth = 0
    for ch in value:
        if ch == '(':
            depth += 1
            current.append(ch)
        elif ch == ')':
            depth -= 1
            current.append(ch)
            if depth == 0 and current:
                tokens.append(''.join(current))
                current = []
            continue
        elif ch in (' ', '\t') and depth == 0:
            if current:
                tokens.append(''.join(current))
                current = []
            continue
        else:
            current.append(ch)
    if current:
        tokens.append(''.join(current))

    for tok in tokens:
        tl = tok.lower()
        if tl.startswith('url(') or tl.startswith('linear-gradient(') or tl.startswith('radial-gradient(') or tl.startswith('repeating-linear-gradient(') or tl.startswith('repeating-radial-gradient('):
            result['background-image'] = tok
        elif tl in _BG_REPEATS:
            result['background-repeat'] = tok
        elif tl in _BG_ATTACHMENTS:
            result['background-attachment'] = tok
        elif tl in _BG_POSITIONS_KW:
            existing = result.get('background-position', '')
            result['background-position'] = (existing + ' ' + tok).strip()
        elif re.match(r'^-?\d+(\.\d+)?(px|em|rem|%)?$', tl):
            existing = result.get('background-position', '')
            result['background-position'] = (existing + ' ' + tok).strip()
        elif _is_color_token(tl):
            result['background-color'] = tok

    return result


# -----------------------------------------------------------------------
# Shorthand expansion
# -----------------------------------------------------------------------

SHORTHAND_EXPANSIONS = {
    'margin':       ['margin-top', 'margin-right', 'margin-bottom', 'margin-left'],
    'padding':      ['padding-top', 'padding-right', 'padding-bottom', 'padding-left'],
    'border-width': ['border-top-width', 'border-right-width',
                     'border-bottom-width', 'border-left-width'],
    'border-style': ['border-top-style', 'border-right-style',
                     'border-bottom-style', 'border-left-style'],
    'border-color': ['border-top-color', 'border-right-color',
                     'border-bottom-color', 'border-left-color'],
    'border-radius': ['border-top-left-radius', 'border-top-right-radius',
                      'border-bottom-right-radius', 'border-bottom-left-radius'],
}

# border-side shorthand names
_BORDER_SIDES = {
    'border-top':    ('border-top-width',    'border-top-style',    'border-top-color'),
    'border-right':  ('border-right-width',  'border-right-style',  'border-right-color'),
    'border-bottom': ('border-bottom-width', 'border-bottom-style', 'border-bottom-color'),
    'border-left':   ('border-left-width',   'border-left-style',   'border-left-color'),
}


def _apply_4side_shorthand(targets: list, parts: list) -> dict:
    """Expand a 1-4 value shorthand to 4 longhands (TRBL pattern)."""
    result = {}
    n = len(parts)
    if n == 1:
        for t in targets:
            result[t] = parts[0]
    elif n == 2:
        result[targets[0]] = parts[0]
        result[targets[1]] = parts[1]
        result[targets[2]] = parts[0]
        result[targets[3]] = parts[1]
    elif n == 3:
        result[targets[0]] = parts[0]
        result[targets[1]] = parts[1]
        result[targets[2]] = parts[2]
        result[targets[3]] = parts[1]
    else:  # 4+
        for i, t in enumerate(targets[:4]):
            result[t] = parts[i]
    return result


def expand_shorthand(prop: str, value: str) -> dict:
    """Expand shorthand properties to individual properties.

    Returns dict of {prop: value} pairs.
    If not a shorthand, returns {prop: value}.
    """
    # ---- Standard 4-side shorthands (margin, padding, border-*) ----
    if prop in SHORTHAND_EXPANSIONS:
        targets = SHORTHAND_EXPANSIONS[prop]
        parts = value.split()
        return _apply_4side_shorthand(targets, parts if parts else [value])

    # ---- border (all 4 sides: width + style + color) ----
    if prop == 'border':
        width, style, color = _parse_border_value(value)
        result = {}
        for side in ('top', 'right', 'bottom', 'left'):
            if width is not None:
                result[f'border-{side}-width'] = width
            if style is not None:
                result[f'border-{side}-style'] = style
            if color is not None:
                result[f'border-{side}-color'] = color
        if not result:
            # fallback: pass through to all 4 sides as-is
            for side in ('top', 'right', 'bottom', 'left'):
                result[f'border-{side}-width'] = value
        return result

    # ---- border-top / border-right / border-bottom / border-left ----
    if prop in _BORDER_SIDES:
        w_prop, s_prop, c_prop = _BORDER_SIDES[prop]
        width, style, color = _parse_border_value(value)
        result = {}
        if width is not None:
            result[w_prop] = width
        if style is not None:
            result[s_prop] = style
        if color is not None:
            result[c_prop] = color
        if not result:
            result[w_prop] = value
        return result

    # ---- font ----
    if prop == 'font':
        parsed = _parse_font_value(value)
        if parsed:
            return parsed
        return {prop: value}

    # ---- background ----
    if prop == 'background':
        parsed = _parse_background_value(value)
        if parsed:
            return parsed
        # fallback: at least set background-color
        return {'background-color': value}

    # ---- outline ----
    if prop == 'outline':
        return _parse_outline_value(value)

    # ---- gap ----
    if prop == 'gap':
        parts = value.split()
        if len(parts) == 1:
            return {'row-gap': parts[0], 'column-gap': parts[0]}
        elif len(parts) >= 2:
            return {'row-gap': parts[0], 'column-gap': parts[1]}
        return {'row-gap': value, 'column-gap': value}

    # ---- flex-flow ----
    if prop == 'flex-flow':
        parts = value.split()
        result = {}
        _directions = {'row', 'row-reverse', 'column', 'column-reverse'}
        _wraps = {'nowrap', 'wrap', 'wrap-reverse'}
        for part in parts:
            pl = part.lower()
            if pl in _directions:
                result['flex-direction'] = part
            elif pl in _wraps:
                result['flex-wrap'] = part
        if not result:
            result['flex-direction'] = value
        return result

    # ---- flex ----
    if prop == 'flex':
        parts = value.split()
        if len(parts) == 1:
            v = parts[0].lower()
            if v == 'none':
                return {'flex-grow': '0', 'flex-shrink': '0', 'flex-basis': 'auto'}
            if v == 'auto':
                return {'flex-grow': '1', 'flex-shrink': '1', 'flex-basis': 'auto'}
            # single number → flex-grow
            try:
                float(v)
                return {'flex-grow': parts[0], 'flex-shrink': '1', 'flex-basis': '0'}
            except ValueError:
                pass
            return {'flex-basis': parts[0]}
        elif len(parts) == 2:
            # flex-grow flex-shrink  OR  flex-grow flex-basis
            try:
                float(parts[1])
                return {'flex-grow': parts[0], 'flex-shrink': parts[1], 'flex-basis': '0'}
            except ValueError:
                return {'flex-grow': parts[0], 'flex-basis': parts[1]}
        elif len(parts) >= 3:
            return {
                'flex-grow':   parts[0],
                'flex-shrink': parts[1],
                'flex-basis':  parts[2],
            }
        return {prop: value}

    # ---- list-style ----
    if prop == 'list-style':
        _LST = {'disc', 'circle', 'square', 'decimal', 'decimal-leading-zero',
                'lower-roman', 'upper-roman', 'lower-alpha', 'upper-alpha',
                'lower-latin', 'upper-latin', 'none'}
        _LSP = {'inside', 'outside'}
        result = {}
        for tok in value.split():
            tl = tok.lower()
            if tl in _LST:
                result['list-style-type'] = tok
            elif tl in _LSP:
                result['list-style-position'] = tok
            elif tl.startswith('url(') or tl == 'none':
                result.setdefault('list-style-image', tok)
        if not result:
            result['list-style-type'] = value
        return result

    # ---- transition ----
    if prop == 'transition':
        # Store as-is; individual longhands are tracked separately
        return {prop: value}

    # ---- animation ----
    if prop == 'animation':
        # Store as-is
        return {prop: value}

    # ---- columns ----
    if prop == 'columns':
        parts = value.split()
        result = {}
        for part in parts:
            tl = part.lower()
            if tl == 'auto':
                # ambiguous, set both
                result.setdefault('column-count', 'auto')
                result.setdefault('column-width', 'auto')
            elif re.match(r'^\d+$', tl):
                result['column-count'] = part
            else:
                result['column-width'] = part
        return result or {prop: value}

    # Not a shorthand
    return {prop: value}
