"""Central dispatch: map CSS display values to LayoutEngine instances."""
from __future__ import annotations
from layout.box import BoxModel
from layout.context import LayoutContext


def _get_display(node) -> str:
    if hasattr(node, 'style') and node.style:
        return node.style.get('display', 'block')
    return 'block'


def layout_node(node, container: BoxModel, ctx: LayoutContext) -> BoxModel | None:
    """Dispatch to the right layout engine. Returns None for display:none."""
    from html.dom import Element
    if not isinstance(node, Element):
        return None

    display = _get_display(node)
    if display == 'none':
        return None

    position = (node.style or {}).get('position', 'static')

    # Absolutely/fixed positioned elements are deferred
    if position in ('absolute', 'fixed'):
        return None

    engine = _get_engine(display)
    box = engine.layout(node, container, ctx)
    if box is not None:
        node.box = box
    return box


def _get_engine(display: str):
    """Return the LayoutEngine instance for the given display value."""
    # Import lazily to avoid circular imports
    if display == 'flex':
        from layout.flex import FlexLayout
        return FlexLayout()
    if display == 'grid':
        from layout.grid import GridLayout
        return GridLayout()
    if display == 'table':
        from layout.table import TableLayout
        return TableLayout()
    # block, list-item, table-row, table-cell, table-row-group, etc. → BlockLayout
    from layout.block import BlockLayout
    return BlockLayout()
