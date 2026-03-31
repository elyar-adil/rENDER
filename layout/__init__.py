"""Layout engine entry point."""
import logging
_logger = logging.getLogger(__name__)
from layout.box import BoxModel, EdgeSizes, Rect
from layout.block import layout_block, layout_absolute
from layout.flex import layout_flex
from layout.inline import layout_inline
from layout.grid import layout_grid
from layout.paint import build_display_list as _build_display_list
from layout.links import _extract_links
from rendering.display_list import DisplayList


VIEWPORT_WIDTH = 980
VIEWPORT_HEIGHT = 600


def layout(document, viewport_width: int = VIEWPORT_WIDTH, viewport_height: int = VIEWPORT_HEIGHT) -> 'DisplayList':
    """Layout the document and return a DisplayList of draw commands.

    Also sets document.box and element.box on each node.
    """
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
    stacking_top.sort(key=lambda item: (item[0], item[1]))
    for _z, _order, cmds in stacking_top:
        for cmd in cmds:
            display_list.add(cmd)

    return display_list


def _node_style(node, prop: str, default: str) -> str:
    style = getattr(node, 'style', None)
    return style.get(prop, default) if style else default


def _layout_children(node, container_box: BoxModel, viewport_width: int) -> None:
    from html.dom import Element
    from layout.float_manager import FloatManager

    float_mgr = FloatManager()

    for child in node.children:
        if not isinstance(child, Element):
            continue

        display = _node_style(child, 'display', 'block')
        position = _node_style(child, 'position', 'static')

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
        position = _node_style(child, 'position', 'static')
        if position in ('absolute', 'fixed') and getattr(child, 'box', None) is None:
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
    if _node_style(node, 'position', 'static') == 'fixed':
        return root_box
    # Walk parent pointers to find nearest positioned ancestor
    parent = getattr(node, 'parent', None)
    while parent is not None and isinstance(parent, Element):
        if _node_style(parent, 'position', 'static') in ('relative', 'absolute', 'fixed', 'sticky'):
            if hasattr(parent, 'box') and parent.box is not None:
                return parent.box
        parent = getattr(parent, 'parent', None)
    return root_box
