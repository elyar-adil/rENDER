"""Inline formatting context and line boxes."""
from dataclasses import dataclass, field
from layout.box import BoxModel
from layout.text import measure_word, measure_text, get_font, _parse_px

_ATOMIC_INLINE_DISPLAYS = frozenset({'inline-block', 'inline-flex', 'inline-grid'})
_BORDER_WIDTH_KEYWORDS = frozenset({'thin', 'medium', 'thick'})


@dataclass
class InlineItem:
    """Represents a single inline unit (word, or replaced element like img)."""
    text: str
    x: float = 0.0
    y: float = 0.0
    width: float = 0.0
    height: float = 0.0
    color: str = 'black'
    font_family: str = 'Arial'
    font_size: float = 16.0
    font_weight: str = 'normal'
    font_italic: bool = False
    decoration: str = 'none'
    word_spacing: float = 0.0
    # for legacy compat with old layout code
    type: str = 'WORD'
    # image support: holds a QImage (or None for text items)
    qimage: object = None
    # <a> ancestor element, if any (for link click detection)
    origin_node: object = None
    # Atomic inline formatting context node (e.g. display:inline-block)
    layout_node: object = None
    # Simple form controls rendered from inline layout
    control_type: str = ''
    background_color: str = 'transparent'
    border_color: str = '#767676'
    border_width: float = 1.0
    control_value: str = ''

    @property
    def word(self):
        return self.text

    @property
    def font(self):
        weight = 'bold' if self.font_weight in ('bold', '700') else ''
        return (self.font_family, int(self.font_size))

    @property
    def box(self):
        b = BoxModel()
        b.x = self.x
        b.y = self.y
        b.content_width = self.width
        b.content_height = self.height
        return b


class LineBox:
    """A single line of inline content."""

    def __init__(self, x: float, y: float, available_width: float, min_height: float = 0.0):
        self.x = x
        self.y = y
        self.available_width = available_width
        self.min_height = min_height
        self.items: list[InlineItem] = []
        self.width: float = 0.0
        self.height: float = 0.0

    def add_item(self, item: InlineItem) -> bool:
        """Try to add item. Returns False if it doesn't fit."""
        if self.items and self.width + item.width > self.available_width:
            return False
        item.x = self.x + self.width
        self.items.append(item)
        self.width += item.width
        self.height = max(self.height, item.height)
        return True

    def finalize(self, text_align: str = 'left') -> None:
        """Adjust item positions for text alignment."""
        if not self.items:
            return
        # Apply minimum line height
        if self.min_height > self.height:
            self.height = self.min_height
        slack = self.available_width - self.width
        if text_align == 'center':
            offset = slack / 2
            for item in self.items:
                item.x += offset
        elif text_align == 'right':
            for item in self.items:
                item.x += slack
        # Vertically align all items to baseline (bottom of line)
        for item in self.items:
            item.y = self.y + (self.height - item.height)
            if item.layout_node is not None:
                _shift_layout_subtree(item.layout_node, item.x, item.y)


def _shift_layout_subtree(node, dx: float, dy: float) -> None:
    if dx == 0.0 and dy == 0.0:
        return

    from html.dom import Element

    if hasattr(node, 'box') and node.box is not None:
        node.box.x += dx
        node.box.y += dy

    if hasattr(node, 'line_boxes'):
        for lb in node.line_boxes:
            lb.x += dx
            lb.y += dy
            for item in lb.items:
                item.x += dx
                item.y += dy
                if item.layout_node is not None and item.layout_node is not node:
                    _shift_layout_subtree(item.layout_node, dx, dy)

    for child in getattr(node, 'children', []):
        if isinstance(child, Element):
            _shift_layout_subtree(child, dx, dy)


def _compute_line_height(node, max_font_size: float) -> float:
    """Compute the CSS line-height for a node as an absolute pixel value."""
    lh_val = 'normal'
    if hasattr(node, 'style') and node.style:
        lh_val = node.style.get('line-height', 'normal')

    if lh_val == 'normal' or not lh_val:
        return 1.2 * max_font_size
    if lh_val.endswith('px'):
        try:
            return float(lh_val[:-2])
        except ValueError:
            pass
    # Unitless multiplier (e.g. '1.5')
    try:
        return float(lh_val) * max_font_size
    except ValueError:
        pass
    # percentage (e.g. '150%')
    if lh_val.endswith('%'):
        try:
            return float(lh_val[:-1]) / 100 * max_font_size
        except ValueError:
            pass
    return 1.2 * max_font_size


def layout_inline(node, container_x: float, container_y: float,
                  container_width: float, float_mgr=None) -> tuple[list[LineBox], float]:
    """Layout all inline children of node into line boxes.

    Returns (line_boxes, total_height).
    """
    items = _collect_inline_items(node, container_width)
    if not items:
        return [], 0.0

    text_align = 'left'
    white_space = 'normal'
    text_overflow = 'clip'
    text_indent = 0.0
    if hasattr(node, 'style') and node.style:
        text_align = node.style.get('text-align', 'left')
        white_space = node.style.get('white-space', 'normal')
        text_overflow = node.style.get('text-overflow', 'clip')
        indent_str = node.style.get('text-indent', '')
        if indent_str and indent_str not in ('0', '0px', ''):
            try:
                text_indent = _parse_px(indent_str)
            except Exception:
                text_indent = 0.0

    nowrap = white_space in ('nowrap', 'pre', 'pre-line', 'pre-wrap')

    # Compute dominant font size for line-height calculation
    font_sizes = [item.font_size for item in items if item.font_size > 0]
    max_font_size = max(font_sizes, default=16.0)
    min_line_height = _compute_line_height(node, max_font_size)

    lines = []
    current_y = container_y
    first_line = True

    if nowrap:
        # Put everything in a single line regardless of width
        if float_mgr:
            line_x, line_w = float_mgr.available_rect(current_y, 20, container_x, container_width)
        else:
            line_x, line_w = container_x, container_width

        if text_indent != 0.0:
            line_x += text_indent
            line_w -= text_indent

        line = LineBox(line_x, current_y, line_w, min_height=min_line_height)
        for item in items:
            item.x = line_x + line.width
            line.items.append(item)
            line.width += item.width
            line.height = max(line.height, item.height)

        # text-overflow: ellipsis — truncate if needed
        if text_overflow == 'ellipsis' and line.width > line_w and line.items:
            _apply_ellipsis(line, line_w)

        if line.height == 0:
            line.height = min_line_height or 16
        line.finalize(text_align)
        lines.append(line)
        current_y += line.height
    else:
        i = 0
        while i < len(items):
            # Get available width at current_y (considering floats)
            if float_mgr:
                line_x, line_w = float_mgr.available_rect(current_y, 20, container_x, container_width)
            else:
                line_x, line_w = container_x, container_width

            # Apply text-indent on the first line only
            if first_line and text_indent != 0.0:
                line_x += text_indent
                line_w -= text_indent
                first_line = False

            line = LineBox(line_x, current_y, line_w, min_height=min_line_height)

            placed_any = False
            while i < len(items):
                item = items[i]
                if line.add_item(item):
                    i += 1
                    placed_any = True
                else:
                    if not placed_any:
                        # Force place item (single word wider than line)
                        item.x = line_x
                        line.items.append(item)
                        line.width += item.width
                        line.height = max(line.height, item.height)
                        i += 1
                    break

            if line.height == 0:
                line.height = min_line_height or 16

            line.finalize(text_align)
            lines.append(line)
            current_y += line.height

    total_height = current_y - container_y
    return lines, total_height


def _apply_ellipsis(line: LineBox, available_width: float) -> None:
    """Truncate line items so total width fits within available_width with '...' appended."""
    ellipsis_width = 0.0
    # Estimate ellipsis width using the first item's font metrics
    if line.items:
        first = line.items[0]
        try:
            from layout.text import measure_text
            ew, _ = measure_text('...', first.font_family, first.font_size, first.font_weight, first.font_italic)
            ellipsis_width = ew
        except Exception:
            ellipsis_width = first.font_size * 1.2  # rough estimate

    budget = available_width - ellipsis_width
    kept = []
    total_w = 0.0
    for item in line.items:
        if total_w + item.width <= budget:
            kept.append(item)
            total_w += item.width
        else:
            break

    # Add ellipsis item
    if line.items:
        ref = line.items[0]
        from dataclasses import replace
        ellipsis_item = InlineItem(
            text='...',
            x=line.x + total_w,
            width=ellipsis_width,
            height=ref.height,
            color=ref.color,
            font_family=ref.font_family,
            font_size=ref.font_size,
            font_weight=ref.font_weight,
            font_italic=ref.font_italic,
        )
        kept.append(ellipsis_item)

    line.items = kept
    line.width = sum(item.width for item in kept)


def _collect_inline_items(node, container_width: float) -> list[InlineItem]:
    """Walk DOM tree collecting inline items from text and atomic inline boxes."""
    items = []
    style = node.style if hasattr(node, 'style') and node.style else {}
    _collect(node, items, style, container_width, is_root=True, current_link=None)
    return items


def _resolve_img_dim(css_val: str, attr_val: str, natural: int) -> int:
    """Return pixel dimension for an img width/height. Returns 0 if unknown."""
    for val in (css_val, attr_val):
        if not val or val in ('auto', ''):
            continue
        try:
            if val.endswith('px'):
                return int(float(val[:-2]))
            if val.lstrip('-').isdigit():
                return int(val)
            return int(_parse_px(val))
        except Exception:
            pass
    return natural or 0


def _resolve_inline_block_width(style: dict, container_width: float) -> float | None:
    width = (style or {}).get('width', 'auto')
    if not width or width == 'auto':
        return None
    try:
        if width.endswith('%'):
            return max(0.0, container_width * float(width[:-1]) / 100.0)
        return max(0.0, _parse_px(width))
    except Exception:
        return None


def _measure_text_span(text: str, style: dict) -> float:
    family = style.get('font-family', 'Arial')
    size_px = _parse_px(style.get('font-size', '16px'))
    weight = style.get('font-weight', 'normal')
    italic = style.get('font-style', 'normal') in ('italic', 'oblique')
    try:
        width, _ = measure_text(text, family, size_px, weight, italic)
        return width
    except Exception:
        return max(0.0, len(text) * size_px * 0.6)


def _measure_inline_block_intrinsic_width(node, inherited_style: dict) -> float:
    from html.dom import Element, Text

    width = 0.0

    if isinstance(node, Text):
        text = ' '.join(node.data.split())
        if text:
            width += _measure_text_span(text, inherited_style)
        return width

    if isinstance(node, Element):
        style = node.style or inherited_style
        if style.get('display', 'inline') == 'none':
            return 0.0
        if style.get('position', 'static') in ('absolute', 'fixed'):
            return 0.0
        if style.get('float', 'none') != 'none':
            return 0.0
        if node.tag == 'img':
            nat_w = getattr(node, 'natural_width', 0) or 0
            content_width = float(_resolve_img_dim(style.get('width', ''), node.attributes.get('width', ''), nat_w))
        elif node.tag == 'input':
            input_type = node.attributes.get('type', 'text').lower().strip()
            if input_type == 'hidden':
                return 0.0
            content_width = 0.0
            width_str = style.get('width', '')
            if width_str and width_str not in ('auto', ''):
                try:
                    content_width = _parse_px(width_str)
                except Exception:
                    content_width = 0.0
            if content_width <= 0.0:
                size_attr = node.attributes.get('size', '').strip()
                chars = 20
                if size_attr.isdigit():
                    chars = max(1, int(size_attr))
                family = style.get('font-family', 'Arial')
                size_px = _parse_px(style.get('font-size', '16px'))
                weight = style.get('font-weight', 'normal')
                italic = style.get('font-style', 'normal') in ('italic', 'oblique')
                char_w, _ = measure_text('0' * chars, family, size_px, weight, italic)
                content_width = char_w + 8.0
        else:
            for child in node.children:
                width += _measure_inline_block_intrinsic_width(child, style)
            specified_width = _resolve_inline_block_width(style, 0.0)
            if specified_width is not None:
                width = max(width, specified_width)

            margin_left = _parse_px(style.get('margin-left', '0px'))
            margin_right = _parse_px(style.get('margin-right', '0px'))
            padding_left = _parse_px(style.get('padding-left', '0px'))
            padding_right = _parse_px(style.get('padding-right', '0px'))
            border_left = _parse_px(style.get('border-left-width', '0px'))
            border_right = _parse_px(style.get('border-right-width', '0px'))
            return width + margin_left + margin_right + padding_left + padding_right + border_left + border_right

        margin_left = _parse_px(style.get('margin-left', '0px'))
        margin_right = _parse_px(style.get('margin-right', '0px'))
        padding_left = _parse_px(style.get('padding-left', '0px'))
        padding_right = _parse_px(style.get('padding-right', '0px'))
        border_left = _parse_px(style.get('border-left-width', '0px'))
        border_right = _parse_px(style.get('border-right-width', '0px'))
        return content_width + margin_left + margin_right + padding_left + padding_right + border_left + border_right

    return width


def _resolve_input_text(node) -> str:
    value = node.attributes.get('value', '')
    if value:
        return value
    return node.attributes.get('placeholder', '')


def _build_text_input_item(node, style: dict, current_link) -> InlineItem | None:
    input_type = node.attributes.get('type', 'text').lower().strip()
    if input_type in ('hidden', 'checkbox', 'radio', 'file', 'range', 'color'):
        return None

    family = style.get('font-family', 'Arial')
    size_px = _parse_px(style.get('font-size', '16px'))
    if size_px <= 0:
        size_px = 16.0
    weight = style.get('font-weight', 'normal')
    italic = style.get('font-style', 'normal') in ('italic', 'oblique')

    width = 0.0
    width_str = style.get('width', '')
    if width_str and width_str not in ('auto', ''):
        try:
            width = _parse_px(width_str)
        except Exception:
            width = 0.0
    if width <= 0.0:
        size_attr = node.attributes.get('size', '').strip()
        chars = 20
        if size_attr.isdigit():
            chars = max(1, int(size_attr))
        char_w, _ = measure_text('0' * chars, family, size_px, weight, italic)
        width = char_w + 8.0

    height = 0.0
    height_str = style.get('height', '')
    if height_str and height_str not in ('auto', ''):
        try:
            height = _parse_px(height_str)
        except Exception:
            height = 0.0
    if height <= 0.0:
        _, text_h = measure_text('Hg', family, size_px, weight, italic)
        height = max(text_h + 6.0, size_px + 6.0)

    background_color = style.get('background-color', 'white')
    if background_color in ('transparent', '', 'none'):
        background_color = 'white'
    border_color = style.get('border-color', '#767676')
    border_width = max(1.0, _parse_px(style.get('border-width', '1px')))
    color = style.get('color', 'black')

    return InlineItem(
        text='',
        width=max(0.0, width),
        height=max(0.0, height),
        color=color,
        font_family=family,
        font_size=size_px,
        font_weight=weight,
        font_italic=italic,
        origin_node=current_link,
        type='INPUT',
        control_type='text-input',
        background_color=background_color,
        border_color=border_color,
        border_width=border_width,
        control_value=_resolve_input_text(node),
    )


def _should_promote_atomic_inline(node, style: dict, is_root: bool) -> bool:
    if is_root:
        return False
    display = (style or {}).get('display', 'inline')
    if display in _ATOMIC_INLINE_DISPLAYS:
        return True
    if display not in _BLOCK_DISPLAYS:
        return False
    if any(getattr(child, 'tag', None) for child in getattr(node, 'children', [])):
        return False
    if any(getattr(child, 'data', '').strip() for child in getattr(node, 'children', [])):
        return False
    if any(
        (style or {}).get(prop, '') not in ('', 'none', 'auto', 'transparent', '0', '0px')
        for prop in ('width', 'height', 'background-image', 'background-color')
    ):
        return True
    return _has_visible_border(style or {})


def _has_visible_border(style: dict) -> bool:
    for side in ('top', 'right', 'bottom', 'left'):
        border_style = style.get(f'border-{side}-style', style.get('border-style', 'none'))
        if border_style in ('', 'none', 'hidden'):
            continue
        width = style.get(f'border-{side}-width', style.get('border-width', '0'))
        if width in _BORDER_WIDTH_KEYWORDS:
            return True
        try:
            if _parse_px(width) > 0.0:
                return True
        except Exception:
            pass
    return False


def _layout_inline_block(node, inherited_style: dict, container_width: float):
    from layout.block import BlockLayout
    from layout.context import LayoutContext
    from layout.flex import FlexLayout
    from layout.grid import GridLayout

    style = node.style or inherited_style
    display = style.get('display', 'inline-block')
    resolved_width = _resolve_inline_block_width(style, container_width)
    if resolved_width is None:
        resolved_width = _measure_inline_block_intrinsic_width(node, style)

    resolved_width = max(0.0, min(resolved_width, container_width)) if container_width > 0 else max(0.0, resolved_width)

    tmp = BoxModel()
    tmp.x = 0.0
    tmp.y = 0.0
    tmp.content_width = resolved_width if resolved_width > 0 else max(container_width, 0.0)
    tmp.content_height = 0.0

    original_width = style.get('width') if isinstance(style, dict) else None
    width_was_auto = original_width in (None, '', 'auto')
    if width_was_auto and resolved_width > 0:
        outer_non_content = (
            _parse_px(style.get('margin-left', '0px')) +
            _parse_px(style.get('margin-right', '0px')) +
            _parse_px(style.get('padding-left', '0px')) +
            _parse_px(style.get('padding-right', '0px')) +
            _parse_px(style.get('border-left-width', '0px')) +
            _parse_px(style.get('border-right-width', '0px'))
        )
        style['width'] = f'{max(0.0, resolved_width - outer_non_content)}px'

    try:
        ctx = LayoutContext(max(int(container_width), 1))
        if display == 'inline-flex':
            box = FlexLayout().layout(node, tmp, ctx)
        elif display == 'inline-grid':
            box = GridLayout().layout(node, tmp, ctx)
        else:
            box = BlockLayout().layout(node, tmp, ctx)
    finally:
        if width_was_auto:
            style.pop('width', None)
        elif original_width is not None:
            style['width'] = original_width

    return box


_BLOCK_DISPLAYS = frozenset({
    'block', 'flex', 'grid', 'table', 'list-item',
    'table-row', 'table-cell', 'table-header-group',
    'table-footer-group', 'table-row-group',
    'table-caption', 'table-column-group',
})


def _collect(node, items: list, inherited_style: dict = None, container_width: float = 0.0, is_root: bool = False,
             current_link=None) -> None:
    """Collect inline items from node.

    For the root block container (is_root=True) we recurse into all children
    but skip block-level element children (they are laid out separately by BFC).
    For inline element children we recurse, inheriting their style.
    current_link: the nearest <a> ancestor element (or None).
    """
    from html.dom import Element, Text

    if inherited_style is None:
        inherited_style = {}

    if isinstance(node, Text):
        style = inherited_style
        text = node.data
        if not text.strip():
            return

        family = style.get('font-family', 'Arial')
        size_px = _parse_px(style.get('font-size', '16px'))
        weight = style.get('font-weight', 'normal')
        italic = style.get('font-style', 'normal') in ('italic', 'oblique')
        color = style.get('color', 'black')
        decoration = style.get('text-decoration', 'none')
        word_spacing = _parse_px(style.get('word-spacing', '0px'))

        # letter-spacing support
        letter_spacing_val = style.get('letter-spacing', 'normal')
        if letter_spacing_val == 'normal' or not letter_spacing_val:
            letter_spacing = 0.0
        else:
            try:
                letter_spacing = _parse_px(letter_spacing_val)
            except Exception:
                letter_spacing = 0.0

        words = text.split()
        space_w, _ = measure_text(' ', family, size_px, weight, italic)
        for word in words:
            w, h = measure_text(word, family, size_px, weight, italic)
            # Add letter-spacing contribution
            if letter_spacing != 0.0:
                w += letter_spacing * len(word)
            item = InlineItem(
                text=word,
                width=w + space_w + word_spacing,
                height=h,
                color=color,
                font_family=family,
                font_size=size_px,
                font_weight=weight,
                font_italic=italic,
                decoration=decoration,
                word_spacing=word_spacing,
                origin_node=current_link,
            )
            items.append(item)

    elif isinstance(node, Element):
        style = node.style or inherited_style
        display = style.get('display', 'inline')
        position = style.get('position', 'static')
        float_val = style.get('float', 'none')
        if display == 'none':
            return
        if not is_root and position in ('absolute', 'fixed'):
            return
        if node.tag == 'img':
            if not is_root and display in _BLOCK_DISPLAYS and current_link is None:
                return
            if float_val != 'none':
                return
            qimage = getattr(node, 'qimage', None)
            nat_w = getattr(node, 'natural_width', 0)
            nat_h = getattr(node, 'natural_height', 0)
            w = _resolve_img_dim(style.get('width', ''), node.attributes.get('width', ''), nat_w)
            h = _resolve_img_dim(style.get('height', ''), node.attributes.get('height', ''), nat_h)
            if w and not h and nat_h and nat_w:
                h = int(w * nat_h / nat_w)
            elif h and not w and nat_w and nat_h:
                w = int(h * nat_w / nat_h)
            if w == 0:
                w = nat_w or 0
            if h == 0:
                h = nat_h or 0
            if w > 0 and h > 0:
                item = InlineItem(
                    text='',
                    width=float(w),
                    height=float(h),
                    qimage=qimage,
                    type='IMG',
                    origin_node=current_link,
                )
                items.append(item)
            return
        if node.tag == 'input':
            item = _build_text_input_item(node, style, current_link)
            if item is not None:
                items.append(item)
            return
        # Skip block-level children of the root container — they have their own boxes
        promote_atomic = _should_promote_atomic_inline(node, style, is_root)
        if not is_root and display in _BLOCK_DISPLAYS and not promote_atomic:
            return
        # Skip floated elements — they are handled by BFC, not inline flow
        if float_val != 'none':
            return

        # Atomic inline formatting contexts
        if promote_atomic:
            box = _layout_inline_block(node, inherited_style, container_width)
            node.box = box
            item = InlineItem(
                text='',
                x=box.x,
                y=box.y,
                width=box.margin.left + box.border.left + box.padding.left + box.content_width
                      + box.padding.right + box.border.right + box.margin.right,
                height=box.margin.top + box.border.top + box.padding.top + box.content_height
                       + box.padding.bottom + box.border.bottom + box.margin.bottom,
                origin_node=current_link,
                layout_node=node,
                type='INLINE-BLOCK',
            )
            items.append(item)
            return

        # Determine link context for children
        link = node if node.tag == 'a' else current_link

        child_style = style
        for child in node.children:
            _collect(child, items, child_style, container_width, is_root=False, current_link=link)

    else:
        for child in node.children:
            _collect(child, items, inherited_style, container_width, is_root=False, current_link=current_link)
