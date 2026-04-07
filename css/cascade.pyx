"""CSS cascade: applies styles from all sources to every Element in the DOM."""
from __future__ import annotations

import re
import logging
import sys
import threading
from urllib.parse import urlparse
from html.dom import Document, Element, Text, Node
import css.parser as css_parser
import css.selector as selector_mod
from css.properties import PROPERTIES, expand_shorthand
from css.utils import split_paren_aware
from collections import defaultdict

_logger = logging.getLogger(__name__)
_FONT_FACE_CACHE: dict[str, bool] = {}
_FONT_FACE_LOCK = threading.Lock()
_SFNT_MAGIC = {b'\x00\x01\x00\x00', b'OTTO', b'true', b'ttcf'}
_SAFE_FONT_EXTS = {'.ttf', '.otf', '.ttc', '.otc'}
_VAR_SHORTHANDS = {'border', 'border-top', 'border-right', 'border-bottom', 'border-left'}
_RE_MEDIA_AND = re.compile(r'(?i)\band\b\s*')
_RE_CAPITALIZE = re.compile(r'(^|[\s\t\r\n\f\v]+)(\S)')


def bind(document: Document, ua_css_path: str,
         viewport_width: int = 980, viewport_height: int = 600,
         extra_css_texts: list = None, base_url: str = '') -> None:
    """Apply computed styles to every Element in the DOM tree.

    extra_css_texts: optional list of raw CSS strings (e.g. fetched external
    stylesheets) to include in the cascade after the UA sheet and inline styles.
    """
    ua_rules = _load_ua(ua_css_path)
    doc_rules = _extract_doc_styles(document)
    extra_rules = []
    if extra_css_texts:
        for css_text in extra_css_texts:
            if css_text:
                try:
                    stylesheet = css_parser.parse_stylesheet(css_text)
                    extra_rules.extend(stylesheet.rules)
                except Exception as exc:
                    _logger.debug('External stylesheet parse error: %s', exc)

    # Load @font-face fonts from all rule sources
    all_rules = ua_rules + doc_rules + extra_rules
    _load_font_faces(all_rules, base_url)

    # Tag each rule with its cascade origin:
    #   0 = user-agent (lowest priority for normal declarations)
    #   1 = author     (highest priority for normal declarations)
    # Per CSS spec, origin takes precedence over specificity.
    tagged_rules = (
        [(0, r) for r in ua_rules] +
        [(1, r) for r in doc_rules + extra_rules]
    )

    # Pre-compile + build inverted index for fast matching
    index = _build_index(tagged_rules, viewport_width, viewport_height)

    # Walk DOM iteratively
    _apply_iterative(document, index)

    # Generate ::before / ::after pseudo-element nodes
    _generate_pseudo_elements(document)

    # Inheritance pass
    _inherit_iterative(document)

    # CSS custom property (var()) resolution
    _resolve_vars_iterative(document)

    # Text-transform post-pass for renderable text nodes
    _apply_text_transform_iterative(document)


# ---------------------------------------------------------------------------
# Rule loading
# ---------------------------------------------------------------------------

def _load_font_faces(rules: list, base_url: str) -> None:
    """Parse @font-face rules and register fonts via QFontDatabase."""
    if sys.platform.startswith('win'):
        return
    try:
        from PyQt6.QtGui import QFontDatabase
        from PyQt6.QtCore import QByteArray
    except ImportError:
        return
    for rule in rules:
        if hasattr(rule, 'name') and rule.name == 'font-face':
            family = None
            src_value = None
            for decl in getattr(rule, 'declarations', []):
                if decl.property == 'font-family':
                    family = decl.value.strip().strip('"\'')
                elif decl.property == 'src':
                    src_value = decl.value
            src_url = _pick_font_face_src(src_value) if src_value else None
            if family and src_url:
                try:
                    from network.http import fetch_bytes, resolve_url
                    url = resolve_url(base_url, src_url) if base_url else src_url
                    with _FONT_FACE_LOCK:
                        cached = _FONT_FACE_CACHE.get(url)
                    if cached is False:
                        continue
                    if cached is True:
                        continue
                    font_data = fetch_bytes(url)
                    if not _looks_like_sfnt_font(font_data):
                        with _FONT_FACE_LOCK:
                            _FONT_FACE_CACHE[url] = False
                        continue
                    font_id = QFontDatabase.addApplicationFontFromData(QByteArray(font_data))
                    with _FONT_FACE_LOCK:
                        _FONT_FACE_CACHE[url] = font_id >= 0
                except Exception as exc:
                    with _FONT_FACE_LOCK:
                        _FONT_FACE_CACHE[url] = False
                    _logger.debug('Ignoring @font-face load failure for %s: %s', src_url, exc)
        # Recurse into @media blocks
        if hasattr(rule, 'rules') and rule.rules:
            _load_font_faces(rule.rules, base_url)


def _pick_font_face_src(src_value: str | None) -> str | None:
    """Prefer TrueType/OpenType sources and skip formats that are unsafe in Qt."""
    if not src_value:
        return None
    matches = re.findall(
        r"url\(['\"]?([^'\")\s]+)['\"]?\)\s*(?:format\(['\"]?([^'\")]+)['\"]?\))?",
        src_value,
        re.I,
    )
    if not matches:
        return None

    fallback = None
    for raw_url, fmt in matches:
        parsed = urlparse(raw_url)
        ext = ''
        if parsed.path:
            _, _, ext = parsed.path.rpartition('.')
            ext = f'.{ext.lower()}' if ext else ''
        fmt = (fmt or '').strip().lower()
        if ext in _SAFE_FONT_EXTS or fmt in ('truetype', 'opentype', 'collection'):
            return raw_url
        if fallback is None:
            fallback = raw_url
    return fallback


def _looks_like_sfnt_font(raw: bytes) -> bool:
    """Accept only SFNT-based fonts to avoid Qt/DirectWrite crashes on Windows."""
    if not raw or len(raw) < 4:
        return False
    return raw[:4] in _SFNT_MAGIC


def _load_ua(path: str) -> list:
    try:
        with open(path, encoding='utf-8') as f:
            stylesheet = css_parser.parse_stylesheet(f.read())
        return list(stylesheet.rules)
    except Exception as exc:
        _logger.warning('Failed to load UA stylesheet %s: %s', path, exc)
        return []


def _extract_doc_styles(node) -> list:
    rules = []
    stack = [node]
    while stack:
        n = stack.pop()
        if isinstance(n, Element) and n.tag == 'style':
            css_text = ''.join(c.data for c in n.children if isinstance(c, Text))
            if css_text:
                try:
                    stylesheet = css_parser.parse_stylesheet(css_text)
                    rules.extend(stylesheet.rules)
                except Exception as exc:
                    _logger.debug('Style block parse error: %s', exc)
        stack.extend(n.children)
    return rules


# ---------------------------------------------------------------------------
# @media query evaluation
# ---------------------------------------------------------------------------

def _media_matches(prelude: str, viewport_width: int, viewport_height: int) -> bool:
    """Evaluate a @media prelude string against the given viewport dimensions.

    Supports:
      - all, screen  → True
      - print        → False
      - (min-width: Xpx), (max-width: Xpx), (min-height: Xpx), (max-height: Xpx)
      - Conditions joined by 'and'
      - 'not' prefix to invert
      - Comma-separated (OR logic)
    """
    raw = prelude.strip()
    if not raw:
        return True

    # Comma-separated → OR
    for part in split_paren_aware(raw):
        if _media_single_matches(part.strip(), viewport_width, viewport_height):
            return True
    return False


def _media_single_matches(query: str, viewport_width: int, viewport_height: int) -> bool:
    """Evaluate a single (non-comma) media query."""
    query = query.strip()
    negated = False
    if query.lower().startswith('not '):
        negated = True
        query = query[4:].strip()

    # Split by 'and' (outside parens)
    conditions = _split_media_and(query)
    result = all(
        _media_condition_matches(c.strip(), viewport_width, viewport_height)
        for c in conditions
    )
    return not result if negated else result


def _split_media_and(text: str) -> list:
    """Split by 'and' keyword outside parentheses."""
    # Tokenize by parenthesised groups first
    result = []
    depth = 0
    current = []
    i = 0
    while i < len(text):
        if text[i] == '(':
            depth += 1
            current.append(text[i])
            i += 1
        elif text[i] == ')':
            depth -= 1
            current.append(text[i])
            i += 1
        elif depth == 0 and text[i:i+4].lower() == 'and ' and (not current or current[-1].strip() == ''):
            # 'and' keyword outside parens
            part = ''.join(current).strip()
            if part:
                result.append(part)
            current = []
            i += 4
        elif depth == 0 and _RE_MEDIA_AND.match(text[i:]):
            # 'and' with word boundary
            m = _RE_MEDIA_AND.match(text[i:])
            if m:
                part = ''.join(current).strip()
                if part:
                    result.append(part)
                current = []
                i += m.end()
                continue
            current.append(text[i])
            i += 1
        else:
            current.append(text[i])
            i += 1
    remaining = ''.join(current).strip()
    if remaining:
        result.append(remaining)
    return result if result else [text]


def _media_condition_matches(cond: str, viewport_width: int, viewport_height: int) -> bool:
    """Evaluate a single media condition token (e.g. 'screen', '(min-width: 768px)')."""
    cond = cond.strip().lower()
    if not cond:
        return True
    if cond.startswith('only '):
        cond = cond[5:].strip()
    if cond in ('all', 'screen'):
        return True
    if cond in ('print', 'handheld', 'speech', 'tv', 'projection', 'tty', 'braille', 'embossed'):
        return False
    if cond.startswith('(') and cond.endswith(')'):
        return _media_feature_matches(cond[1:-1].strip(), viewport_width, viewport_height)
    return False


def _media_feature_matches(feature: str, viewport_width: int, viewport_height: int) -> bool:
    """Evaluate a media feature expression inside parentheses."""
    m = re.match(r'([\w-]+)\s*:\s*(-?\d+(?:\.\d+)?)(px|em|rem)?', feature)
    if not m:
        return False

    name = m.group(1)
    value = _media_length_to_px(float(m.group(2)), m.group(3) or 'px')

    if name in ('min-width', 'min-device-width'):
        return viewport_width >= value
    if name in ('max-width', 'max-device-width'):
        return viewport_width <= value
    if name in ('min-height', 'min-device-height'):
        return viewport_height >= value
    if name in ('max-height', 'max-device-height'):
        return viewport_height <= value
    return False


def _media_length_to_px(value: float, unit: str) -> float:
    """Convert CSS media-query lengths to px."""
    unit = unit.lower()
    if unit == 'px':
        return value
    if unit in ('em', 'rem'):
        return value * 16.0
    return value


# ---------------------------------------------------------------------------
# Inverted index for fast selector matching
# ---------------------------------------------------------------------------

def _build_index(tagged_rules: list, viewport_width: int = 980,
                 viewport_height: int = 600) -> dict:
    """Build an inverted index: tag/class/id -> [(origin, spec, order, sel_text, decls)].

    tagged_rules is a list of (origin, rule) pairs where origin is:
        0 = user-agent stylesheet
        1 = author stylesheet
    @media rules are evaluated against the viewport and included/excluded
    accordingly.
    """
    # order counter for stable cascade ordering within the same origin+specificity
    order = 0

    by_tag     = defaultdict(list)  # tag name → entries
    by_class   = defaultdict(list)  # class name → entries
    by_id      = defaultdict(list)  # id → entries
    universal  = []                  # rules that match any element

    def _process_rule(rule, origin):
        nonlocal order
        if hasattr(rule, 'name') and rule.name == 'media':
            # @media rule — evaluate prelude
            if _media_matches(rule.prelude, viewport_width, viewport_height):
                for sub in rule.rules:
                    _process_rule(sub, origin)
            return

        if hasattr(rule, 'name') and rule.name in ('supports', 'layer', 'document'):
            # @supports / @layer — treat as always-true for now
            # (we assume all features are "supported" so @supports passes)
            for sub in getattr(rule, 'rules', []):
                _process_rule(sub, origin)
            return

        if not hasattr(rule, 'declarations'):
            return

        sel_text = rule.prelude.strip()
        for single_sel in split_paren_aware(sel_text):
            single_sel = single_sel.strip()
            if not single_sel:
                continue
            try:
                spec = selector_mod.specificity(single_sel)
                # Sort key: (origin, spec, order) — origin ensures author > UA
                entry = (origin, spec, order, single_sel, rule.declarations)
                order += 1

                key_tag, key_classes, key_id = _extract_subject_keys(single_sel)
                if key_id:
                    by_id[key_id].append(entry)
                elif key_classes:
                    for cls in key_classes:
                        by_class[cls].append(entry)
                elif key_tag and key_tag != '*':
                    by_tag[key_tag].append(entry)
                else:
                    universal.append(entry)
            except Exception as exc:
                _logger.debug('Skipping selector %r: %s', single_sel, exc)

    for origin, rule in tagged_rules:
        _process_rule(rule, origin)

    return {'tag': by_tag, 'class': by_class, 'id': by_id, 'universal': universal}


def _extract_subject_keys(selector_text: str):
    """Extract (tag, [classes], id) from the RIGHTMOST simple selector.

    Fast heuristic parser (no full CSS parse) — good enough for indexing.
    """
    # Strip :pseudo, [attr] to find the bare selector core
    # Remove pseudo-classes/elements and attribute selectors for key extraction
    cleaned = re.sub(r'\[[^\]]*\]', '', selector_text)
    cleaned = re.sub(r'::?[\w-]+(\([^)]*\))?', '', cleaned)

    # Get the rightmost compound selector (after last combinator)
    parts = re.split(r'\s+|(?<=[^\s])>(?=[^\s])|(?<=[^\s])\+(?=[^\s])|(?<=[^\s])~(?=[^\s])', cleaned.strip())
    if not parts:
        return None, [], None
    rightmost = parts[-1].strip()
    if not rightmost:
        rightmost = cleaned.strip()

    tag = None
    classes = []
    id_ = None

    # parse the rightmost compound selector
    tokens = re.findall(r'([#.][^\s#.+>~:[\]]+|[a-zA-Z][a-zA-Z0-9_-]*|\*)', rightmost)
    for tok in tokens:
        if tok.startswith('#'):
            id_ = tok[1:].lower()
        elif tok.startswith('.'):
            classes.append(tok[1:].lower())
        elif tok == '*':
            pass  # universal
        else:
            tag = tok.lower()

    return tag, classes, id_


# ---------------------------------------------------------------------------
# DOM walk
# ---------------------------------------------------------------------------

def _apply_iterative(document, index: dict) -> None:
    stack = [document]
    while stack:
        node = stack.pop()
        if isinstance(node, Element):
            _apply_to_element(node, index)
        stack.extend(reversed(node.children))


# HTML <font size="N"> → CSS font-size mapping (per HTML spec)
_FONT_SIZE_MAP = {
    '1': '10px', '2': '13px', '3': '16px', '4': '18px',
    '5': '24px', '6': '32px', '7': '48px',
}

_BORDER_SIDES = ('top', 'right', 'bottom', 'left')


def _set_border_shorthand(computed, prop, value):
    """Set a border shorthand (border-style, border-width, border-color) with longhand expansion."""
    if prop == 'border-style':
        for side in _BORDER_SIDES:
            computed[f'border-{side}-style'] = value
    elif prop == 'border-width':
        for side in _BORDER_SIDES:
            computed[f'border-{side}-width'] = value
    elif prop == 'border-color':
        for side in _BORDER_SIDES:
            computed[f'border-{side}-color'] = value
    computed[prop] = value


def _apply_html_presentation_hints(node: Element, computed: dict) -> None:
    """Convert HTML presentational attributes to CSS at lowest priority.

    Called before the CSS cascade loop so any CSS rule overrides these.
    """
    attrs = node.attributes
    tag = node.tag

    # bgcolor → background-color
    bg = attrs.get('bgcolor', '').strip()
    if bg:
        computed['background-color'] = bg

    # color on font/body/td → color
    col = attrs.get('color', '').strip()
    if col:
        computed['color'] = col

    # width → width  (e.g. "85%" or "18" → "18px")
    w = attrs.get('width', '').strip()
    if w:
        computed['width'] = w if (w.endswith('%') or w.endswith('px') or w.endswith('em')) else w + 'px'

    # height → height
    h = attrs.get('height', '').strip()
    if h:
        computed['height'] = h if (h.endswith('%') or h.endswith('px') or h.endswith('em')) else h + 'px'

    # align → text-align
    align = attrs.get('align', '').lower().strip()
    if align in ('left', 'right', 'center', 'justify'):
        computed['text-align'] = align
    elif tag == 'td' and 'text-align' not in computed:
        computed['text-align'] = 'left'
    elif tag == 'th' and 'text-align' not in computed:
        computed['text-align'] = 'center'

    # valign → vertical-align
    valign = attrs.get('valign', '').lower().strip()
    if valign in ('top', 'middle', 'bottom', 'baseline'):
        computed['vertical-align'] = valign

    # nowrap on td/th → white-space: nowrap
    if 'nowrap' in attrs:
        computed['white-space'] = 'nowrap'

    # ----- <font> element: size, face, color -----
    if tag == 'font':
        size = attrs.get('size', '').strip()
        if size and size in _FONT_SIZE_MAP:
            computed['font-size'] = _FONT_SIZE_MAP[size]
        face = attrs.get('face', '').strip()
        if face:
            computed['font-family'] = face

    # ----- <body> text/link/vlink attributes -----
    if tag == 'body':
        text_color = attrs.get('text', '').strip()
        if text_color:
            computed['color'] = text_color

    # <a> inherits link color from ancestor <body link="...">
    if tag == 'a':
        p = getattr(node, 'parent', None)
        while p is not None:
            if isinstance(p, Element) and p.tag == 'body':
                link_color = p.attributes.get('link', '').strip()
                if link_color:
                    computed['color'] = link_color
                break
            p = getattr(p, 'parent', None)

    # ----- <hr> size, color, noshade -----
    if tag == 'hr':
        size = attrs.get('size', '').strip()
        if size:
            computed['height'] = size if size.endswith('px') else size + 'px'
        hr_color = attrs.get('color', '').strip()
        if hr_color:
            computed['background-color'] = hr_color
            _set_border_shorthand(computed, 'border-style', 'solid')
            _set_border_shorthand(computed, 'border-color', hr_color)
        if 'noshade' in attrs:
            if computed.get('border-top-style', 'none') == 'none':
                _set_border_shorthand(computed, 'border-style', 'solid')

    # table-specific
    if tag == 'table':
        cs = attrs.get('cellspacing', '').strip()
        if cs:
            val = cs if cs.endswith('px') else cs + 'px'
            computed['border-spacing'] = val
        border = attrs.get('border', '').strip()
        if border == '0':
            _set_border_shorthand(computed, 'border-style', 'none')
        elif border:
            _set_border_shorthand(computed, 'border-style', 'solid')
            _set_border_shorthand(computed, 'border-width',
                                  border if border.endswith('px') else border + 'px')
            if computed.get('border-top-color', 'currentcolor') == 'currentcolor':
                _set_border_shorthand(computed, 'border-color', 'gray')

    # <td>/<th> in a <table border="N"> → cells get 1px inset border
    if tag in ('td', 'th'):
        parent = getattr(node, 'parent', None)
        # Walk up to find the table ancestor
        table_node = None
        p = parent
        while p is not None:
            if isinstance(p, Element) and p.tag == 'table':
                table_node = p
                break
            p = getattr(p, 'parent', None)
        if table_node is not None:
            tb = table_node.attributes.get('border', '').strip()
            if tb and tb != '0':
                if computed.get('border-top-style', 'none') == 'none':
                    _set_border_shorthand(computed, 'border-style', 'solid')
                    _set_border_shorthand(computed, 'border-width', '1px')
                    _set_border_shorthand(computed, 'border-color', 'gray')

    # img border → border
    if tag == 'img':
        border = attrs.get('border', '').strip()
        if border == '0':
            _set_border_shorthand(computed, 'border-style', 'none')
        elif border:
            _set_border_shorthand(computed, 'border-style', 'solid')
            _set_border_shorthand(computed, 'border-width',
                                  border if border.endswith('px') else border + 'px')

    # <center> → text-align:center + auto margins on children
    if tag == 'center':
        computed.setdefault('text-align', 'center')
    parent = getattr(node, 'parent', None)
    if parent is not None and getattr(parent, 'tag', '') == 'center':
        if 'margin-left' not in computed:
            computed['margin-left'] = 'auto'
        if 'margin-right' not in computed:
            computed['margin-right'] = 'auto'


def _apply_to_element(node: Element, index: dict) -> None:
    # Gather candidate rules from the index (fast pre-filter)
    candidates = list(index['universal'])

    # Tag lookup
    if node.tag in index['tag']:
        candidates.extend(index['tag'][node.tag])

    # Class lookup
    cls_attr = node.attributes.get('class', '')
    if cls_attr:
        for cls in cls_attr.split():
            cls_lower = cls.lower()
            if cls_lower in index['class']:
                candidates.extend(index['class'][cls_lower])

    # ID lookup
    node_id = node.attributes.get('id', '')
    if node_id:
        id_lower = node_id.lower()
        if id_lower in index['id']:
            candidates.extend(index['id'][id_lower])

    # Deduplicate by object identity, sort by (origin, spec, order)
    # origin: 0=UA, 1=author — ensures author rules always beat UA rules
    seen = set()
    unique = []
    for entry in candidates:
        eid = id(entry)
        if eid not in seen:
            seen.add(eid)
            unique.append(entry)
    unique.sort(key=lambda x: (x[0], x[1], x[2]))  # (origin, specificity, order)

    ua_computed = {}
    author_computed = {}
    important = {}
    css_vars = {}

    pseudo_rules = {}  # 'before' / 'after' → list of (origin, decls)

    for _origin, spec, _order, sel_text, decls in unique:
        try:
            # Check if this rule targets a pseudo-element
            pe = selector_mod.get_pseudo_element(sel_text)
            if pe in ('before', 'after'):
                # Match the element part (ignoring pseudo-element)
                if selector_mod.matches(node, sel_text):
                    pseudo_rules.setdefault(pe, []).append((_origin, decls))
                continue

            if selector_mod.matches(node, sel_text):
                target = ua_computed if _origin == 0 else author_computed
                for decl in decls:
                    # Collect CSS custom properties separately
                    if decl.property.startswith('--'):
                        css_vars[decl.property] = decl.value
                    else:
                        expanded = expand_shorthand(decl.property, decl.value)
                        if decl.property in _VAR_SHORTHANDS and 'var(' in decl.value:
                            expanded[decl.property] = decl.value
                        if decl.important:
                            important.update(expanded)
                        else:
                            target.update(expanded)
        except Exception as exc:
            _logger.debug('Selector match error for %r on <%s>: %s', sel_text, node.tag, exc)

    # Priority: UA < hints < author < inline < !important
    computed = {**ua_computed}
    _apply_html_presentation_hints(node, computed)  # hints override UA defaults
    computed.update(author_computed)                 # author CSS overrides hints

    # Inline styles (higher priority than author, lower than !important)
    if 'style' in node.attributes and node.attributes['style']:
        try:
            inline = css_parser.parse_inline_style(node.attributes['style'])
            for k, v in inline.items():
                if k.startswith('--'):
                    css_vars[k] = v
                else:
                    expanded = expand_shorthand(k, v)
                    if k in _VAR_SHORTHANDS and 'var(' in v:
                        expanded[k] = v
                    computed.update(expanded)
        except Exception as exc:
            _logger.debug('Inline style parse error on <%s>: %s', node.tag, exc)

    computed.update(important)                       # !important overrides everything

    # Initial values for missing properties.
    # Inherited properties are intentionally left absent here so that
    # _inherit_iterative can fill them from the parent.  Non-inherited
    # properties always fall back to their initial value.
    for prop, propdef in PROPERTIES.items():
        if prop not in computed:
            if not propdef.inherited:
                computed[prop] = propdef.initial
            # inherited props: leave absent → _resolve_inherit fills them

    node.style = computed
    node.css_vars = css_vars
    node._pseudo_rules = pseudo_rules


# ---------------------------------------------------------------------------
# Pseudo-element generation (::before / ::after)
# ---------------------------------------------------------------------------

def _resolve_content(content_val: str, element=None) -> str:
    """Resolve CSS content property value to plain text."""
    if not content_val or content_val in ('none', 'normal'):
        return ''
    # Strip surrounding quotes: "text" or 'text'
    val = content_val.strip()
    if (val.startswith('"') and val.endswith('"')) or \
       (val.startswith("'") and val.endswith("'")):
        return val[1:-1]
    # attr(name)
    if val.startswith('attr(') and val.endswith(')') and element is not None:
        attr_name = val[5:-1].strip()
        return element.attributes.get(attr_name, '')
    # Bare identifier or empty — treat as text
    if val == '""' or val == "''":
        return ''
    return val


def _generate_pseudo_elements(document) -> None:
    """Walk the DOM and create virtual ::before/::after child Elements."""
    stack = [document]
    while stack:
        node = stack.pop()
        if isinstance(node, Element) and hasattr(node, '_pseudo_rules'):
            for pos in ('before', 'after'):
                rule_list = node._pseudo_rules.get(pos)
                if not rule_list:
                    continue
                # Merge all declarations for this pseudo-element
                merged = {}
                for _origin, decls in rule_list:
                    for decl in decls:
                        expanded = expand_shorthand(decl.property, decl.value)
                        merged.update(expanded)
                content = merged.get('content', 'none')
                text = _resolve_content(content, node)
                # Even empty-string content generates the element (for bg/border)
                if content in ('none', 'normal'):
                    continue
                # Ensure display defaults to inline if not specified
                if 'display' not in merged:
                    merged['display'] = 'inline'
                # Fill initial values for non-inherited properties
                for prop, propdef in PROPERTIES.items():
                    if prop not in merged and not propdef.inherited:
                        merged[prop] = propdef.initial
                pseudo = Element(f'__{pos}__')
                pseudo.style = merged
                pseudo.css_vars = {}
                pseudo.parent = node
                if text:
                    t = Text(text)
                    t.parent = pseudo
                    pseudo.children = [t]
                else:
                    pseudo.children = []
                if pos == 'before':
                    node.children.insert(0, pseudo)
                else:
                    node.children.append(pseudo)
        for child in reversed(getattr(node, 'children', [])):
            stack.append(child)


# ---------------------------------------------------------------------------
# Inheritance
# ---------------------------------------------------------------------------

def _inherit_iterative(document) -> None:
    stack = [(document, {})]
    while stack:
        node, parent_style = stack.pop()
        if isinstance(node, Element):
            _resolve_inherit(node, parent_style)
            child_style = node.style
        else:
            child_style = parent_style
        for child in reversed(node.children):
            stack.append((child, child_style))


def _resolve_inherit(node: Element, parent_style: dict) -> None:
    style = node.style
    for prop, propdef in PROPERTIES.items():
        val = style.get(prop)   # None when absent (inherited prop not set by cascade)
        if val == 'inherit' or (propdef.inherited and val is None):
            style[prop] = parent_style.get(prop, propdef.initial)
    # Handle explicit 'inherit' on non-registered or already-set properties
    for prop in list(style.keys()):
        if style.get(prop) == 'inherit':
            propdef = PROPERTIES.get(prop)
            style[prop] = parent_style.get(prop, propdef.initial if propdef else '')


# ---------------------------------------------------------------------------
# CSS custom property (var()) resolution
# ---------------------------------------------------------------------------

_VAR_RE = re.compile(r'var\(\s*(--[\w-]+)\s*(?:,\s*([^)]*))?\s*\)')


def _resolve_vars_iterative(document) -> None:
    """Walk the DOM tree and replace var() references with their resolved values.

    Each element builds its vars dict from its own css_vars merged with
    its parent's resolved vars.
    """
    stack = [(document, {})]
    while stack:
        node, parent_vars = stack.pop()
        if isinstance(node, Element):
            # Build this element's vars dict
            own_vars = getattr(node, 'css_vars', {})
            vars_dict = {**parent_vars, **own_vars}

            # Resolve var() in all style values
            style = getattr(node, 'style', {})
            for prop, val in list(style.items()):
                if val and 'var(' in val:
                    style[prop] = _replace_vars(val, vars_dict)
            _expand_resolved_var_shorthands(style)

            child_vars = vars_dict
        else:
            child_vars = parent_vars

        for child in reversed(node.children):
            stack.append((child, child_vars))


def _apply_text_transform_iterative(document) -> None:
    stack = [(document, 'none', False)]
    while stack:
        node, inherited_transform, suppress_text = stack.pop()
        if isinstance(node, Element):
            style = getattr(node, 'style', {}) or {}
            text_transform = style.get('text-transform', inherited_transform)
            suppress_children = suppress_text or node.tag in ('style', 'script')
            for child in reversed(node.children):
                stack.append((child, text_transform, suppress_children))
        elif isinstance(node, Text):
            if suppress_text:
                continue
            source_text = getattr(node, '_source_data', node.data)
            node._source_data = source_text
            node.data = _transform_text(source_text, inherited_transform)
        else:
            for child in reversed(getattr(node, 'children', [])):
                stack.append((child, inherited_transform, suppress_text))


def _transform_text(text: str, text_transform: str) -> str:
    if not text or not text_transform or text_transform == 'none':
        return text
    if text_transform == 'uppercase':
        return text.upper()
    if text_transform == 'lowercase':
        return text.lower()
    if text_transform == 'capitalize':
        return _RE_CAPITALIZE.sub(lambda m: m.group(1) + m.group(2).upper(), text)
    return text


def _replace_vars(value: str, vars_dict: dict) -> str:
    """Replace all var(--name) and var(--name, fallback) in a value string."""
    def replacer(m):
        name = m.group(1)
        fallback = m.group(2)
        if name in vars_dict:
            resolved = vars_dict[name]
            # Recursively resolve nested vars in the resolved value
            if 'var(' in resolved:
                resolved = _replace_vars(resolved, vars_dict)
            return resolved
        if fallback is not None:
            fb = fallback.strip()
            if 'var(' in fb:
                fb = _replace_vars(fb, vars_dict)
            return fb
        return m.group(0)  # unresolved — leave as-is

    # Iterate to handle nested/chained var() calls (max 10 rounds)
    prev = None
    result = value
    for _ in range(10):
        prev = result
        result = _VAR_RE.sub(replacer, result)
        if result == prev:
            break
    return result


def _expand_resolved_var_shorthands(style: dict) -> None:
    border_value = style.get('border')
    if border_value and border_value not in ('none', '') and 'var(' not in border_value:
        if _border_longhands_look_unresolved(style):
            style.update(expand_shorthand('border', border_value))

    for side in ('top', 'right', 'bottom', 'left'):
        prop = f'border-{side}'
        value = style.get(prop)
        if value and value not in ('none', '') and 'var(' not in value:
            if _border_side_longhands_look_unresolved(style, side):
                style.update(expand_shorthand(prop, value))


def _border_longhands_look_unresolved(style: dict) -> bool:
    return all(_border_side_longhands_look_unresolved(style, side) for side in ('top', 'right', 'bottom', 'left'))


def _border_side_longhands_look_unresolved(style: dict, side: str) -> bool:
    width = style.get(f'border-{side}-width', '')
    border_style = style.get(f'border-{side}-style', '')
    color = style.get(f'border-{side}-color', '')
    return (
        width in ('', 'medium')
        and color in ('', 'currentcolor')
        and border_style in ('', 'none', 'solid')
    )


# ---------------------------------------------------------------------------
# Utility
# ---------------------------------------------------------------------------

